use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{self, IsTerminal};
use std::path::Path;

use anyhow::{anyhow, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Select};

use crate::adapters::Registry;
use crate::context::ProjectContext;
use crate::model::{Candidate, InstallPlan, ManagerInstance, Selection, Target};

pub fn interactive() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

pub struct SelectionRequest<'a> {
    pub registry: &'a Registry,
    pub context: &'a ProjectContext,
    pub instances: &'a [ManagerInstance],
    pub queries: &'a [String],
    pub candidates: &'a [Candidate],
    pub target_override: Option<&'a str>,
    pub instance_override: Option<&'a str>,
    pub allow_prompts: bool,
}

pub fn select_installations(request: SelectionRequest<'_>) -> Result<Vec<Selection>> {
    let SelectionRequest {
        registry,
        context,
        instances,
        queries,
        candidates,
        target_override,
        instance_override,
        allow_prompts,
    } = request;
    let theme = ColorfulTheme::default();
    let mut selections = Vec::new();

    for query in queries {
        let mut options = Vec::new();
        for candidate in candidates
            .iter()
            .filter(|candidate| candidate.query == *query)
            .filter(|candidate| {
                instance_override.is_none_or(|requested| candidate.manager_instance_id == requested)
            })
        {
            let instance = instances
                .iter()
                .find(|instance| instance.id == candidate.manager_instance_id)
                .ok_or_else(|| {
                    anyhow!(
                        "candidate {} refers to unknown manager instance {}",
                        terminal_safe(&candidate.package),
                        terminal_safe(&candidate.manager_instance_id)
                    )
                })?;
            if instance.kind != candidate.manager {
                return Err(anyhow!(
                    "candidate {} claims manager {} but belongs to {}",
                    terminal_safe(&candidate.package),
                    candidate.manager,
                    terminal_safe(&instance.id)
                ));
            }
            if let Some(requested) = target_override {
                let adapter = registry.adapter(instance.kind)?;
                if !adapter
                    .compatible_targets(instance, context)
                    .iter()
                    .any(|target| target_matches(target, requested))
                {
                    continue;
                }
            }
            options.push(candidate.clone());
        }
        if options.is_empty() {
            return Err(anyhow!(
                "no candidate compatible with the requested manager and target was found for {query:?}"
            ));
        }
        sort_candidates(query, &mut options);
        deduplicate_logical_candidates(&mut options);

        let candidate = if allow_prompts {
            let labels = options.iter().map(candidate_label).collect::<Vec<_>>();
            let index = Select::with_theme(&theme)
                .with_prompt(format!("Select manager and package for {query}"))
                .items(&labels)
                .default(0)
                .interact()?;
            options[index].clone()
        } else {
            choose_unattended_candidate(query, &options)?
        };

        let instance = instances
            .iter()
            .find(|instance| instance.id == candidate.manager_instance_id)
            .ok_or_else(|| anyhow!("selected manager instance disappeared"))?;
        let adapter = registry.adapter(instance.kind)?;
        let targets = adapter.compatible_targets(instance, context);
        if targets.is_empty() {
            return Err(anyhow!("{} has no compatible target", instance.id));
        }

        let target = choose_target(query, &targets, target_override, allow_prompts, &theme)?;
        selections.push(Selection {
            query: query.clone(),
            candidate,
            target,
        });
    }

    Ok(selections)
}

fn choose_unattended_candidate(query: &str, options: &[Candidate]) -> Result<Candidate> {
    let instance_ids = options
        .iter()
        .map(|candidate| candidate.manager_instance_id.as_str())
        .collect::<BTreeSet<_>>();
    if instance_ids.len() > 1 {
        let ids = instance_ids
            .into_iter()
            .map(terminal_safe)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow!(
            "{query:?} matched candidates from multiple manager instances ({ids}); pass --instance with one of these ids or run interactively"
        ));
    }

    let Some(best) = options.first() else {
        return Err(anyhow!("no candidate was found for {query:?}"));
    };
    let best_is_exact = exact_package_match(query, best);
    let tied = options
        .iter()
        .take_while(|candidate| {
            exact_package_match(query, candidate) == best_is_exact && candidate.score == best.score
        })
        .collect::<Vec<_>>();
    if tied.len() > 1 {
        let instance_id = terminal_safe(&best.manager_instance_id);
        let packages = tied
            .iter()
            .map(|candidate| terminal_safe(&candidate.package))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow!(
            "{query:?} is ambiguous within manager instance {instance_id}: {} candidates share the best match (score {}): {packages}; refine the package query or run interactively",
            tied.len(),
            best.score
        ));
    }
    Ok(best.clone())
}

fn sort_candidates(query: &str, candidates: &mut [Candidate]) {
    candidates.sort_by(|a, b| {
        (b.package == query)
            .cmp(&(a.package == query))
            .then_with(|| exact_package_match(query, b).cmp(&exact_package_match(query, a)))
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.manager_instance_id.cmp(&b.manager_instance_id))
            .then_with(|| a.manager.cmp(&b.manager))
            .then_with(|| a.package.cmp(&b.package))
            .then_with(|| a.version.cmp(&b.version))
            .then_with(|| b.verified.cmp(&a.verified))
            .then_with(|| a.source.cmp(&b.source))
    });
}

fn deduplicate_logical_candidates(candidates: &mut Vec<Candidate>) {
    let mut seen = BTreeSet::new();
    candidates.retain(|candidate| {
        seen.insert((
            candidate.manager_instance_id.clone(),
            candidate.package.to_ascii_lowercase(),
            candidate.version.clone(),
        ))
    });
}

fn exact_package_match(query: &str, candidate: &Candidate) -> bool {
    candidate.package.eq_ignore_ascii_case(query)
}

fn choose_target(
    query: &str,
    targets: &[Target],
    target_override: Option<&str>,
    allow_prompts: bool,
    theme: &ColorfulTheme,
) -> Result<Target> {
    if let Some(requested) = target_override {
        return targets
            .iter()
            .find(|target| target_matches(target, requested))
            .cloned()
            .ok_or_else(|| anyhow!("target {requested:?} is not compatible with {query:?}"));
    }
    if allow_prompts {
        let labels = targets
            .iter()
            .map(|target| {
                format!(
                    "{} [{}]",
                    terminal_safe(&target.label),
                    terminal_safe(&target.id)
                )
            })
            .collect::<Vec<_>>();
        let index = Select::with_theme(theme)
            .with_prompt(format!("Select installation target for {query}"))
            .items(&labels)
            .default(0)
            .interact()?;
        return Ok(targets[index].clone());
    }
    if targets.len() == 1 {
        return Ok(targets[0].clone());
    }
    Err(anyhow!(
        "{query:?} has {} compatible targets; pass --target or run interactively",
        targets.len()
    ))
}

fn target_matches(target: &Target, requested: &str) -> bool {
    target.id == requested
        || target
            .path
            .as_ref()
            .is_some_and(|path| path.to_string_lossy() == requested)
}

pub fn print_doctor(context: &ProjectContext, instances: &[ManagerInstance], warnings: &[String]) {
    println!("Project: {}", terminal_safe(&context.cwd.to_string_lossy()));
    if let Some(root) = &context.project_root {
        println!("Project root: {}", terminal_safe(&root.to_string_lossy()));
    }
    if context.workspace_root != context.project_root {
        if let Some(root) = &context.workspace_root {
            println!("Workspace root: {}", terminal_safe(&root.to_string_lossy()));
        }
    }
    if !context.markers.is_empty() {
        println!("Markers: {}", terminal_safe(&context.markers.join(", ")));
    }
    println!("\nManagers:");
    if instances.is_empty() {
        println!("  (none found)");
    }
    for instance in instances {
        println!(
            "  {:<8} {:<36} {} [{}]",
            instance.kind,
            terminal_safe(&instance.executable.to_string_lossy()),
            terminal_safe(&instance.version),
            terminal_safe(&instance.id)
        );
    }

    println!("\nTargets:");
    for target in &context.targets {
        println!(
            "  {:<36} {}{}",
            terminal_safe(&target.id),
            terminal_safe(&target.label),
            if target.exists { "" } else { " (new)" }
        );
    }
    print_warnings(warnings);
}

pub fn print_candidates(queries: &[String], candidates: &[Candidate], warnings: &[String]) {
    for query in queries {
        println!("\n{}:", terminal_safe(query));
        let mut found = false;
        for candidate in candidates
            .iter()
            .filter(|candidate| candidate.query == *query)
        {
            found = true;
            println!("  {}", candidate_label(candidate));
            if let Some(description) = &candidate.description {
                println!("      {}", terminal_safe(description));
            }
        }
        if !found {
            println!("  (no matches)");
        }
    }
    print_warnings(warnings);
}

pub fn print_plan(plan: &InstallPlan, inherited_cwd: &Path) {
    print!("{}", render_plan(plan, inherited_cwd));
}

fn render_plan(plan: &InstallPlan, inherited_cwd: &Path) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "\nInstallation plan:");
    for selection in &plan.selections {
        let _ = writeln!(
            output,
            "  {} -> {}:{} -> {}",
            terminal_safe(&selection.query),
            selection.candidate.manager,
            terminal_safe(&selection.candidate.package),
            terminal_safe(&selection.target.id)
        );
    }
    let _ = writeln!(output, "\nCommands:");
    for (index, step) in plan.steps.iter().enumerate() {
        let _ = writeln!(
            output,
            "  {}. {}",
            index + 1,
            terminal_safe_complete(&step.display_command())
        );
        let _ = writeln!(
            output,
            "     Label: {}",
            terminal_safe_complete(&step.label)
        );
        let (cwd, cwd_source) = step
            .cwd
            .as_deref()
            .map_or((inherited_cwd, "inherited"), |cwd| (cwd, "explicit"));
        let _ = writeln!(
            output,
            "     Working directory ({cwd_source}): {}",
            terminal_safe_complete(&cwd.to_string_lossy())
        );
        if step.env_remove_prefixes.is_empty() {
            let _ = writeln!(output, "     Unset inherited environment prefixes: (none)");
        } else {
            let mut prefixes = step.env_remove_prefixes.iter().collect::<Vec<_>>();
            prefixes.sort();
            prefixes.dedup();
            let _ = writeln!(
                output,
                "     Unset inherited environment prefixes (every inherited variable whose name starts with the prefix; matching is case-insensitive on Windows):"
            );
            for prefix in prefixes {
                let _ = writeln!(output, "       - {}*", terminal_safe_complete(prefix));
            }
        }
        if step.env.is_empty() {
            let _ = writeln!(output, "     Set/override environment: (none)");
        } else {
            let _ = writeln!(
                output,
                "     Set/override environment (applied after prefix removals):"
            );
            for (key, value) in &step.env {
                let _ = writeln!(
                    output,
                    "       - {}={}",
                    terminal_safe_complete(key),
                    terminal_safe_complete(value)
                );
            }
        }
        if let Some(path) = &step.must_not_exist {
            let _ = writeln!(
                output,
                "     Plan-wide creation/overwrite guard: before any command, abort the entire plan if this filesystem entry exists, including as a broken symlink (and check again before this step): {}",
                terminal_safe_complete(&path.to_string_lossy())
            );
        } else {
            let _ = writeln!(
                output,
                "     Plan-wide creation/overwrite guard: (no must-not-exist path precondition for this step)"
            );
        }
        let privileges = if step.requires_admin {
            "administrator/root elevation required"
        } else {
            "user context required; no elevation requested (mate refuses this plan when run as root)"
        };
        let _ = writeln!(output, "     Privileges: {privileges}");
    }
    let _ = writeln!(
        output,
        "\nNo command above is executed until this complete plan is confirmed."
    );
    let _ = writeln!(
        output,
        "All path guards are checked before step 1 and again before their step; steps run serially, stop on the first failure, and inherit terminal input/output."
    );
    let _ = writeln!(
        output,
        "A step requiring admin privileges invokes trusted system sudo when mate is not already running as root; execution fails if it is unavailable."
    );
    output
}

pub fn confirm_plan() -> Result<bool> {
    Ok(Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Execute this plan? Downloads may begin after confirmation")
        .default(false)
        .interact()?)
}

fn candidate_label(candidate: &Candidate) -> String {
    format!(
        "{:<7} {:<28} {:<14} score={}{} @ {} [{}]",
        candidate.manager,
        terminal_safe(&candidate.package),
        terminal_safe(candidate.version.as_deref().unwrap_or("version ?")),
        candidate.score,
        if candidate.verified {
            ""
        } else {
            " unverified"
        },
        terminal_safe(&candidate.manager_instance_id),
        terminal_safe(&candidate.source)
    )
}

fn print_warnings(warnings: &[String]) {
    if warnings.is_empty() {
        return;
    }
    eprintln!("\nWarnings:");
    for warning in warnings {
        eprintln!("  - {}", terminal_safe(warning));
    }
}

fn terminal_safe(value: &str) -> String {
    const MAX_CHARS: usize = 500;
    terminal_safe_with_limit(value, Some(MAX_CHARS))
}

fn terminal_safe_complete(value: &str) -> String {
    terminal_safe_with_limit(value, None)
}

fn terminal_safe_with_limit(value: &str, max_chars: Option<usize>) -> String {
    let mut output = String::new();
    let mut truncated = false;
    for (index, ch) in value.chars().enumerate() {
        if max_chars.is_some_and(|limit| index >= limit) {
            truncated = true;
            break;
        }
        if ch.is_control() {
            output.extend(ch.escape_default());
        } else {
            output.push(ch);
        }
    }
    if truncated {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{render_plan, select_installations, SelectionRequest};
    use crate::adapters::Registry;
    use crate::context::ProjectContext;
    use crate::model::{
        Candidate, CommandSpec, InstallPlan, InstanceScope, ManagerInstance, ManagerKind, Target,
        TargetKind,
    };

    fn system_context() -> ProjectContext {
        ProjectContext {
            cwd: PathBuf::from("/project"),
            project_root: None,
            workspace_root: None,
            targets: vec![Target {
                id: "system".into(),
                kind: TargetKind::System,
                label: "system packages".into(),
                path: None,
                exists: true,
            }],
            markers: Vec::new(),
        }
    }

    fn apt_instance(id: &str) -> ManagerInstance {
        ManagerInstance {
            id: id.into(),
            kind: ManagerKind::Apt,
            executable: PathBuf::from("/usr/bin/apt-get"),
            launcher_args: Vec::new(),
            version: "apt 3".into(),
            scope: InstanceScope::Generic,
        }
    }

    fn apt_candidate(query: &str, package: &str, instance: &str, score: u16) -> Candidate {
        Candidate {
            query: query.into(),
            package: package.into(),
            manager_instance_id: instance.into(),
            manager: ManagerKind::Apt,
            source: "APT configured repositories".into(),
            version: None,
            description: None,
            score,
            verified: true,
        }
    }

    #[test]
    fn unattended_selection_accepts_multiple_candidates_from_one_instance() {
        let registry = Registry::standard();
        let context = system_context();
        let instances = vec![apt_instance("apt:one")];
        let queries = vec!["ripgrep".to_owned()];
        let candidates = vec![
            apt_candidate("ripgrep", "ripgrep-all", "apt:one", 80),
            apt_candidate("ripgrep", "RIPGREP", "apt:one", 100),
            apt_candidate("ripgrep", "ripgrep", "apt:one", 100),
            apt_candidate("ripgrep", "elpa-rg", "apt:one", 60),
        ];

        let selections = select_installations(SelectionRequest {
            registry: &registry,
            context: &context,
            instances: &instances,
            queries: &queries,
            candidates: &candidates,
            target_override: Some("system"),
            instance_override: None,
            allow_prompts: false,
        })
        .unwrap();

        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].candidate.package, "ripgrep");
        assert_eq!(selections[0].candidate.manager_instance_id, "apt:one");
    }

    #[test]
    fn unattended_selection_requires_instance_for_multiple_instances() {
        let registry = Registry::standard();
        let context = system_context();
        let instances = vec![apt_instance("apt:one"), apt_instance("apt:two")];
        let queries = vec!["ripgrep".to_owned()];
        let candidates = vec![
            apt_candidate("ripgrep", "ripgrep", "apt:one", 100),
            apt_candidate("ripgrep", "ripgrep", "apt:two", 100),
        ];

        let error = select_installations(SelectionRequest {
            registry: &registry,
            context: &context,
            instances: &instances,
            queries: &queries,
            candidates: &candidates,
            target_override: Some("system"),
            instance_override: None,
            allow_prompts: false,
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains("multiple manager instances"));
        assert!(error.contains("--instance"));
        assert!(error.contains("apt:one"));
        assert!(error.contains("apt:two"));
    }

    #[test]
    fn instance_override_allows_multiple_packages_from_the_selected_instance() {
        let registry = Registry::standard();
        let context = system_context();
        let instances = vec![apt_instance("apt:one"), apt_instance("apt:two")];
        let queries = vec!["ripgrep".to_owned()];
        let candidates = vec![
            apt_candidate("ripgrep", "ripgrep-all", "apt:one", 80),
            apt_candidate("ripgrep", "ripgrep", "apt:one", 100),
            apt_candidate("ripgrep", "ripgrep", "apt:two", 100),
        ];

        let selections = select_installations(SelectionRequest {
            registry: &registry,
            context: &context,
            instances: &instances,
            queries: &queries,
            candidates: &candidates,
            target_override: Some("system"),
            instance_override: Some("apt:one"),
            allow_prompts: false,
        })
        .unwrap();

        assert_eq!(selections[0].candidate.package, "ripgrep");
        assert_eq!(selections[0].candidate.manager_instance_id, "apt:one");
    }

    #[test]
    fn unattended_selection_reports_tied_packages_without_suggesting_instance() {
        let registry = Registry::standard();
        let context = system_context();
        let instances = vec![apt_instance("apt:one")];
        let queries = vec!["rip".to_owned()];
        let candidates = vec![
            apt_candidate("rip", "ripgrep", "apt:one", 80),
            apt_candidate("rip", "ripgrep-all", "apt:one", 80),
            apt_candidate("rip", "other", "apt:one", 60),
        ];

        let error = select_installations(SelectionRequest {
            registry: &registry,
            context: &context,
            instances: &instances,
            queries: &queries,
            candidates: &candidates,
            target_override: Some("system"),
            instance_override: None,
            allow_prompts: false,
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains("ambiguous within manager instance apt:one"));
        assert!(error.contains("ripgrep"));
        assert!(error.contains("ripgrep-all"));
        assert!(!error.contains("--instance"));
    }

    #[test]
    fn target_filter_can_disambiguate_manager_instances() {
        let registry = Registry::standard();
        let first_path = PathBuf::from("/venvs/first");
        let second_path = PathBuf::from("/venvs/second");
        let context = ProjectContext {
            cwd: PathBuf::from("/project"),
            project_root: None,
            workspace_root: None,
            targets: vec![
                Target {
                    id: "venv:first".into(),
                    kind: TargetKind::PythonVenv,
                    label: "first venv".into(),
                    path: Some(first_path.clone()),
                    exists: true,
                },
                Target {
                    id: "venv:second".into(),
                    kind: TargetKind::PythonVenv,
                    label: "second venv".into(),
                    path: Some(second_path.clone()),
                    exists: true,
                },
            ],
            markers: Vec::new(),
        };
        let make_instance = |id: &str, path: PathBuf| ManagerInstance {
            id: id.into(),
            kind: ManagerKind::Pip,
            executable: path.join("bin/pip"),
            launcher_args: Vec::new(),
            version: "pip 25".into(),
            scope: InstanceScope::PythonVenv(path),
        };
        let instances = vec![
            make_instance("pip:first", first_path),
            make_instance("pip:second", second_path),
        ];
        let queries = vec!["requests".to_owned()];
        let make_candidate = |instance: &str| Candidate {
            query: "requests".into(),
            package: "requests".into(),
            manager_instance_id: instance.into(),
            manager: ManagerKind::Pip,
            source: "https://pypi.org/simple".into(),
            version: Some("2.32.4".into()),
            description: None,
            score: 100,
            verified: true,
        };
        let candidates = vec![make_candidate("pip:second"), make_candidate("pip:first")];

        let selections = select_installations(SelectionRequest {
            registry: &registry,
            context: &context,
            instances: &instances,
            queries: &queries,
            candidates: &candidates,
            target_override: Some("venv:first"),
            instance_override: None,
            allow_prompts: false,
        })
        .unwrap();

        assert_eq!(selections[0].candidate.manager_instance_id, "pip:first");
        assert_eq!(selections[0].target.id, "venv:first");
    }

    #[test]
    fn plan_render_discloses_command_environment_and_guards_deterministically() {
        let mut env = BTreeMap::new();
        env.insert("ZETA".into(), "last".into());
        env.insert("ALPHA".into(), "first\nline".into());
        let long_argument = "x".repeat(600);
        let plan = InstallPlan {
            selections: Vec::new(),
            steps: vec![
                CommandSpec {
                    label: "create environment\u{1b}".into(),
                    program: PathBuf::from("/usr/bin/tool"),
                    args: vec![
                        "install".into(),
                        "hello world".into(),
                        long_argument.clone(),
                    ],
                    cwd: None,
                    env,
                    env_remove_prefixes: vec!["Z_".into(), "A_".into(), "A_".into()],
                    must_not_exist: Some(PathBuf::from("/project/.venv")),
                    requires_admin: true,
                },
                CommandSpec {
                    label: "project install".into(),
                    program: PathBuf::from("/usr/bin/tool"),
                    args: vec!["sync".into()],
                    cwd: Some(PathBuf::from("/workspace/app")),
                    env: BTreeMap::new(),
                    env_remove_prefixes: Vec::new(),
                    must_not_exist: None,
                    requires_admin: false,
                },
            ],
        };

        let rendered = render_plan(&plan, PathBuf::from("/actual/cwd").as_path());

        assert!(rendered.contains("/usr/bin/tool install 'hello world'"));
        assert!(rendered.contains(long_argument.as_str()));
        assert!(rendered.contains("Label: create environment\\u{1b}"));
        assert!(rendered.contains("Working directory (inherited): /actual/cwd"));
        assert!(rendered.contains("Working directory (explicit): /workspace/app"));
        assert!(rendered.contains(
            "Unset inherited environment prefixes (every inherited variable whose name starts with the prefix; matching is case-insensitive on Windows):"
        ));
        assert_eq!(rendered.matches("       - A_*").count(), 1);
        assert!(rendered.find("       - A_*").unwrap() < rendered.find("       - Z_*").unwrap());
        assert!(rendered.contains("Set/override environment (applied after prefix removals):"));
        assert!(
            rendered.find("       - ALPHA=").unwrap() < rendered.find("       - ZETA=").unwrap()
        );
        assert!(rendered.contains("ALPHA=first\\nline"));
        assert!(rendered.contains(
            "Plan-wide creation/overwrite guard: before any command, abort the entire plan if this filesystem entry exists, including as a broken symlink (and check again before this step): /project/.venv"
        ));
        assert!(rendered.contains("Privileges: administrator/root elevation required"));
        assert!(rendered.contains("All path guards are checked before step 1"));
        assert!(rendered.contains("steps run serially, stop on the first failure"));
        assert!(!rendered.contains('\u{1b}'));
    }
}
