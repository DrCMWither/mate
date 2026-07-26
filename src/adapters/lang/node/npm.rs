use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde_json::Value;

use super::{discover_launchers, owned_env_removals, NodeLauncher, NODE_ENV_REMOVALS};
use crate::adapters::{score_match, validate_package_id, validate_query, ManagerAdapter};
use crate::context::ProjectContext;
use crate::model::{
    Candidate, CommandSpec, InstanceScope, ManagerInstance, ManagerKind, Target, TargetKind,
};
use crate::platform;
use crate::process::{probe_version_sanitized, safe_diagnostic, search_command_sanitized};

const NPM_REGISTRY: &str = "https://registry.npmjs.org/";

pub struct NpmAdapter;

#[async_trait]
impl ManagerAdapter for NpmAdapter {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Npm
    }

    async fn discover(&self, context: &ProjectContext) -> Result<Vec<ManagerInstance>> {
        let mut instances = Vec::new();
        for launcher in discover_launchers("npm", "npm-cli.js", context) {
            let version_args = launcher.prepend_launcher(vec!["--version".into()]);
            let version = probe_version_sanitized(
                &launcher.program,
                &version_args,
                NODE_ENV_REMOVALS,
                &[("NPM_CONFIG_COLOR", "false")],
            )
            .await;
            let scope = discover_global_prefix(&launcher, context)
                .await
                .map(InstanceScope::NodeGlobal)
                .unwrap_or(InstanceScope::Generic);
            instances.push(ManagerInstance {
                id: launcher.instance_id("npm"),
                kind: self.kind(),
                executable: launcher.program,
                launcher_args: launcher.launcher_args,
                version,
                scope,
            });
        }
        Ok(instances)
    }

    async fn search(&self, instance: &ManagerInstance, query: &str) -> Result<Vec<Candidate>> {
        validate_query(query)?;
        let args = instance.prepend_launcher(vec![
            "search".into(),
            "--json".into(),
            "--searchlimit=40".into(),
            format!("--registry={NPM_REGISTRY}"),
            "--color=false".into(),
            query.into(),
        ]);
        let output = search_command_sanitized(
            &instance.executable,
            &args,
            NODE_ENV_REMOVALS,
            &[
                ("NPM_CONFIG_REGISTRY", NPM_REGISTRY),
                ("NPM_CONFIG_COLOR", "false"),
                ("NPM_CONFIG_AUDIT", "false"),
                ("NPM_CONFIG_FUND", "false"),
                ("NPM_CONFIG_USERCONFIG", platform::null_device()),
            ],
        )
        .await?;
        if !output.success {
            return Err(anyhow!(
                "npm search failed: {}",
                safe_diagnostic(output.stderr.trim())
            ));
        }
        parse_search(&output.stdout, query, &instance.id)
    }

    fn compatible_targets(
        &self,
        instance: &ManagerInstance,
        context: &ProjectContext,
    ) -> Vec<Target> {
        let mut targets = context
            .targets
            .iter()
            .filter(|target| {
                matches!(
                    target.kind,
                    TargetKind::NodeProject | TargetKind::NodeWorkspace
                )
            })
            .cloned()
            .collect::<Vec<_>>();

        if let InstanceScope::NodeGlobal(prefix) = &instance.scope {
            targets.push(node_global_target(prefix));
        }
        targets
    }

    fn plan(
        &self,
        instance: &ManagerInstance,
        packages: &[String],
        target: &Target,
        context: &ProjectContext,
    ) -> Result<Vec<CommandSpec>> {
        if packages.is_empty() {
            return Err(anyhow!("npm install requires at least one package"));
        }
        for package in packages {
            validate_exact_npm_spec(package)?;
        }

        let (cwd, must_not_exist, global_prefix) = match target.kind {
            TargetKind::NodeProject | TargetKind::NodeWorkspace => {
                let path = target
                    .path
                    .as_ref()
                    .ok_or_else(|| anyhow!("Node project target has no filesystem path"))?;
                let is_known_target = context
                    .targets
                    .iter()
                    .any(|known| known.kind == target.kind && known.path.as_ref() == Some(path));
                if !is_known_target {
                    return Err(anyhow!(
                        "Node project target {} is not part of the detected context",
                        target.id
                    ));
                }
                let precondition = (!target.exists).then_some(path.join("node_modules"));
                (Some(path.clone()), precondition, None)
            }
            TargetKind::NodeGlobal => {
                let path = target
                    .path
                    .as_ref()
                    .ok_or_else(|| anyhow!("npm global target has no filesystem path"))?;
                match &instance.scope {
                    InstanceScope::NodeGlobal(prefix) if prefix == path => {}
                    _ => {
                        return Err(anyhow!(
                            "npm instance {} does not own global target {}",
                            instance.id,
                            target.id
                        ));
                    }
                }
                let precondition = (!target.exists).then_some(path.clone());
                (
                    Some(platform::home_dir().unwrap_or_else(|| context.cwd.clone())),
                    precondition,
                    Some(path),
                )
            }
            _ => {
                return Err(anyhow!(
                    "npm only supports Node project, workspace, or global targets"
                ));
            }
        };

        let mut args = vec![
            "install".into(),
            "--ignore-scripts".into(),
            "--no-audit".into(),
            "--no-fund".into(),
            format!("--registry={NPM_REGISTRY}"),
            "--color=false".into(),
        ];
        if let Some(prefix) = global_prefix {
            let prefix = prefix
                .to_str()
                .ok_or_else(|| anyhow!("npm global prefix is not valid UTF-8"))?;
            args.extend(["--global".into(), "--prefix".into(), prefix.into()]);
        } else {
            args.push("--save-exact".into());
        }
        for scope in package_scopes(packages) {
            args.push(format!("--{scope}:registry={NPM_REGISTRY}"));
        }
        args.extend(packages.iter().cloned());

        let env = BTreeMap::from([
            ("NPM_CONFIG_REGISTRY".into(), NPM_REGISTRY.into()),
            ("NPM_CONFIG_COLOR".into(), "false".into()),
            ("NPM_CONFIG_AUDIT".into(), "false".into()),
            ("NPM_CONFIG_FUND".into(), "false".into()),
            ("NPM_CONFIG_IGNORE_SCRIPTS".into(), "true".into()),
            (
                "NPM_CONFIG_USERCONFIG".into(),
                platform::null_device().into(),
            ),
        ]);
        Ok(vec![CommandSpec {
            label: format!("npm install {} package(s)", packages.len()),
            program: instance.executable.clone(),
            args: instance.prepend_launcher(args),
            cwd,
            env,
            env_remove_prefixes: owned_env_removals(),
            must_not_exist,
            requires_admin: false,
        }])
    }
}

async fn discover_global_prefix(
    launcher: &NodeLauncher,
    context: &ProjectContext,
) -> Option<PathBuf> {
    let args = launcher.prepend_launcher(vec![
        "prefix".into(),
        "--global".into(),
        "--color=false".into(),
    ]);
    let output = search_command_sanitized(
        &launcher.program,
        &args,
        NODE_ENV_REMOVALS,
        &[
            ("NPM_CONFIG_COLOR", "false"),
            ("NPM_CONFIG_USERCONFIG", platform::null_device()),
        ],
    )
    .await
    .ok()?;
    if !output.success {
        return None;
    }
    let value = output
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    if value.chars().any(char::is_control) {
        return None;
    }
    let path = PathBuf::from(value);
    let absolute = if path.is_absolute() {
        path
    } else {
        context.cwd.join(path)
    };
    platform::resolve_for_creation(absolute).ok()
}

fn node_global_target(prefix: &Path) -> Target {
    Target {
        id: format!("node-global:{}", prefix.display()),
        kind: TargetKind::NodeGlobal,
        label: format!("npm global prefix {}", prefix.display()),
        path: Some(prefix.to_path_buf()),
        exists: prefix.is_dir(),
    }
}

fn parse_search(text: &str, query: &str, instance_id: &str) -> Result<Vec<Candidate>> {
    let value: Value = serde_json::from_str(text).context("npm search returned invalid JSON")?;
    let entries = value
        .as_array()
        .ok_or_else(|| anyhow!("npm search returned a non-array JSON value"))?;
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let package = entry.get("name")?.as_str()?.trim();
            let version = entry.get("version")?.as_str()?.trim();
            if package.is_empty()
                || version.is_empty()
                || validate_package_id(package).is_err()
                || !valid_npm_package_name(package)
                || semver::Version::parse(version).is_err()
            {
                return None;
            }
            let description = entry
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            Some(Candidate {
                query: query.to_owned(),
                package: package.to_owned(),
                manager_instance_id: instance_id.to_owned(),
                manager: ManagerKind::Npm,
                source: NPM_REGISTRY.into(),
                version: Some(version.to_owned()),
                description,
                score: score_match(query, package),
                verified: true,
            })
        })
        .take(40)
        .collect())
}

fn validate_exact_npm_spec(spec: &str) -> Result<()> {
    validate_package_id(spec)?;
    let Some((package, version)) = spec.rsplit_once('@') else {
        return Err(anyhow!(
            "npm package {spec:?} is not pinned to an exact registry version"
        ));
    };
    if !valid_npm_package_name(package) || semver::Version::parse(version).is_err() {
        return Err(anyhow!(
            "npm package {spec:?} is not pinned to an exact registry version"
        ));
    }
    Ok(())
}

fn valid_npm_package_name(package: &str) -> bool {
    if package.len() > 214 {
        return false;
    }
    if let Some(scoped) = package.strip_prefix('@') {
        scoped.split_once('/').is_some_and(|(scope, name)| {
            valid_npm_name_component(scope) && valid_npm_name_component(name)
        })
    } else {
        valid_npm_name_component(package)
    }
}

fn valid_npm_name_component(component: &str) -> bool {
    !component.is_empty()
        && !component.starts_with('.')
        && !component.starts_with('_')
        && component
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "-._~".contains(ch))
}

fn package_scopes(packages: &[String]) -> BTreeSet<String> {
    packages
        .iter()
        .filter_map(|spec| spec.rsplit_once('@').map(|(package, _)| package))
        .filter_map(|package| package.strip_prefix('@'))
        .filter_map(|package| package.split_once('/').map(|(scope, _)| scope))
        .filter(|scope| !scope.is_empty())
        .map(|scope| format!("@{scope}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{parse_search, validate_exact_npm_spec, NpmAdapter};
    use crate::adapters::ManagerAdapter;
    use crate::context::ProjectContext;
    use crate::model::{InstanceScope, ManagerInstance, ManagerKind, Target, TargetKind};

    #[test]
    fn parses_structured_npm_search_results() {
        let found = parse_search(
            r#"[
                {
                    "name": "typescript",
                    "version": "5.9.2",
                    "description": "TypeScript is a language"
                },
                {"name": "@types/node", "version": "24.1.0"},
                {"name": "missing-version"}
            ]"#,
            "typescript",
            "npm:test",
        )
        .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].package, "typescript");
        assert_eq!(found[0].version.as_deref(), Some("5.9.2"));
        assert_eq!(found[0].score, 100);
        assert_eq!(found[1].package, "@types/node");
    }

    #[test]
    fn accepts_only_exact_registry_specs() {
        assert!(validate_exact_npm_spec("typescript@5.9.2").is_ok());
        assert!(validate_exact_npm_spec("@types/node@24.1.0").is_ok());
        assert!(validate_exact_npm_spec("typescript").is_err());
        assert!(validate_exact_npm_spec("typescript@latest").is_err());
        assert!(validate_exact_npm_spec("@types/node").is_err());
    }

    #[test]
    fn project_plan_preserves_the_node_launcher_and_safety_flags() {
        let path = PathBuf::from("node-project");
        let target = Target {
            id: "node-project:test".into(),
            kind: TargetKind::NodeProject,
            label: "Node project".into(),
            path: Some(path.clone()),
            exists: false,
        };
        let context = ProjectContext {
            cwd: path.clone(),
            project_root: Some(path.clone()),
            workspace_root: Some(path.clone()),
            targets: vec![target.clone()],
            markers: vec!["package.json".into()],
        };
        let instance = ManagerInstance {
            id: "npm:node".into(),
            kind: ManagerKind::Npm,
            executable: PathBuf::from("node"),
            launcher_args: vec!["npm-cli.js".into()],
            version: "11.9.0".into(),
            scope: InstanceScope::NodeGlobal(PathBuf::from("npm-prefix")),
        };

        let plan = NpmAdapter
            .plan(
                &instance,
                &["typescript@5.9.2".into(), "@types/node@24.1.0".into()],
                &target,
                &context,
            )
            .unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].args[0], "npm-cli.js");
        assert!(plan[0].args.iter().any(|arg| arg == "--save-exact"));
        assert!(plan[0].args.iter().any(|arg| arg == "--ignore-scripts"));
        assert!(plan[0]
            .args
            .iter()
            .any(|arg| arg == "--@types:registry=https://registry.npmjs.org/"));
        assert_eq!(plan[0].cwd.as_ref(), Some(&path));
        assert_eq!(
            plan[0].must_not_exist.as_deref(),
            Some(path.join("node_modules").as_path())
        );
    }

    #[test]
    fn global_plan_is_bound_to_the_discovered_prefix() {
        let prefix = PathBuf::from("npm-prefix");
        let target = Target {
            id: "node-global:test".into(),
            kind: TargetKind::NodeGlobal,
            label: "npm global prefix".into(),
            path: Some(prefix.clone()),
            exists: true,
        };
        let context = ProjectContext {
            cwd: PathBuf::from("project"),
            project_root: None,
            workspace_root: None,
            targets: Vec::new(),
            markers: Vec::new(),
        };
        let instance = ManagerInstance {
            id: "npm:test".into(),
            kind: ManagerKind::Npm,
            executable: PathBuf::from("npm"),
            launcher_args: Vec::new(),
            version: "11.9.0".into(),
            scope: InstanceScope::NodeGlobal(prefix.clone()),
        };

        let plan = NpmAdapter
            .plan(&instance, &["typescript@5.9.2".into()], &target, &context)
            .unwrap();
        let prefix_index = plan[0]
            .args
            .iter()
            .position(|arg| arg == "--prefix")
            .unwrap();
        assert!(plan[0].args.iter().any(|arg| arg == "--global"));
        assert_eq!(plan[0].args[prefix_index + 1], prefix.to_string_lossy());
    }
}
