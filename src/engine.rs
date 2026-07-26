use std::collections::BTreeSet;

use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::adapters::{validate_query, Registry};
use crate::context::ProjectContext;
use crate::model::{Candidate, ManagerInstance, ManagerKind};
use crate::{planner, process, ui};

#[derive(Serialize)]
struct DoctorReport<'a> {
    context: &'a ProjectContext,
    managers: &'a [ManagerInstance],
    warnings: &'a [String],
}

#[derive(Serialize)]
struct SearchReport<'a> {
    packages: &'a [String],
    candidates: &'a [Candidate],
    warnings: &'a [String],
}

pub async fn doctor(json: bool) -> Result<()> {
    let mut context = ProjectContext::discover()?;
    let registry = Registry::standard();
    let (instances, warnings) = registry.discover(&context, &[]).await;
    include_instance_targets(&registry, &instances, &mut context)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DoctorReport {
                context: &context,
                managers: &instances,
                warnings: &warnings,
            })?
        );
    } else {
        ui::print_doctor(&context, &instances, &warnings);
    }
    Ok(())
}

fn include_instance_targets(
    registry: &Registry,
    instances: &[ManagerInstance],
    context: &mut ProjectContext,
) -> Result<()> {
    for instance in instances {
        let adapter = registry.adapter(instance.kind)?;
        for target in adapter.compatible_targets(instance, context) {
            if !context.targets.iter().any(|known| known.id == target.id) {
                context.targets.push(target);
            }
        }
    }
    Ok(())
}

pub async fn search(packages: Vec<String>, managers: Vec<ManagerKind>, json: bool) -> Result<()> {
    let packages = normalize_packages(packages)?;
    let context = ProjectContext::discover()?;
    let registry = Registry::standard();
    registry.ensure_searchable(&managers)?;
    let (instances, mut warnings) = registry.discover(&context, &managers).await;
    ensure_managers_found(&instances, &managers)?;
    let (candidates, search_warnings) = registry.search(&instances, &packages).await;
    warnings.extend(search_warnings);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&SearchReport {
                packages: &packages,
                candidates: &candidates,
                warnings: &warnings,
            })?
        );
    } else {
        ui::print_candidates(&packages, &candidates, &warnings);
    }
    Ok(())
}

pub async fn install(
    packages: Vec<String>,
    managers: Vec<ManagerKind>,
    target: Option<String>,
    instance: Option<String>,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    let packages = normalize_packages(packages)?;
    let context = ProjectContext::discover()?;
    let registry = Registry::standard();
    registry.ensure_installable(&managers)?;
    let (instances, discovery_warnings) = registry.discover(&context, &managers).await;
    ensure_managers_found(&instances, &managers)?;

    let (candidates, search_warnings) = registry.search(&instances, &packages).await;
    let mut warnings = discovery_warnings;
    warnings.extend(search_warnings);
    if !warnings.is_empty() {
        ui::print_candidates(&packages, &candidates, &warnings);
    }
    if yes && !warnings.is_empty() {
        return Err(anyhow!(
            "refusing unattended selection because {} discovery/search warning(s) occurred",
            warnings.len()
        ));
    }

    let prompts = ui::interactive() && !yes;
    if !prompts && managers.is_empty() {
        return Err(anyhow!(
            "non-interactive installation requires --manager to make selection explicit"
        ));
    }
    if !prompts && target.is_none() {
        return Err(anyhow!(
            "non-interactive installation requires --target; inspect ids with `mate doctor`"
        ));
    }

    let selections = ui::select_installations(ui::SelectionRequest {
        registry: &registry,
        context: &context,
        instances: &instances,
        queries: &packages,
        candidates: &candidates,
        target_override: target.as_deref(),
        instance_override: instance.as_deref(),
        allow_prompts: prompts,
    })?;
    let plan = planner::build_plan(&registry, &instances, selections, &context)?;
    ui::print_plan(&plan, &context.cwd);

    if dry_run {
        println!("\nDry run complete; nothing was installed.");
        return Ok(());
    }

    if !yes {
        if !ui::interactive() {
            return Err(anyhow!(
                "cannot confirm outside an interactive terminal; pass --yes only after reviewing a dry run"
            ));
        }
        if !ui::confirm_plan()? {
            println!("Cancelled; nothing was installed.");
            return Ok(());
        }
    }

    process::execute_steps(&plan.steps).await?;
    println!("\nInstallation completed.");
    Ok(())
}

fn normalize_packages(packages: Vec<String>) -> Result<Vec<String>> {
    const MAX_PACKAGES: usize = 64;
    if packages.len() > MAX_PACKAGES {
        return Err(anyhow!(
            "a single batch is limited to {MAX_PACKAGES} package queries"
        ));
    }

    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for package in packages {
        validate_query(&package)?;
        if seen.insert(package.clone()) {
            normalized.push(package);
        }
    }
    Ok(normalized)
}

fn ensure_managers_found(instances: &[ManagerInstance], requested: &[ManagerKind]) -> Result<()> {
    if instances.is_empty() {
        return Err(anyhow!("no supported package managers were discovered"));
    }

    let found = instances
        .iter()
        .map(|instance| instance.kind)
        .collect::<BTreeSet<_>>();
    let missing = requested
        .iter()
        .filter(|kind| !found.contains(kind))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(anyhow!(
            "requested manager(s) not found: {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::include_instance_targets;
    use crate::adapters::Registry;
    use crate::context::ProjectContext;
    use crate::model::{InstanceScope, ManagerInstance, ManagerKind};

    #[test]
    fn doctor_includes_manager_specific_global_targets() {
        let prefix = PathBuf::from("/npm/prefix");
        let instance = ManagerInstance {
            id: "npm:test".into(),
            kind: ManagerKind::Npm,
            executable: PathBuf::from("npm"),
            launcher_args: Vec::new(),
            version: "11.9.0".into(),
            scope: InstanceScope::NodeGlobal(prefix.clone()),
        };
        let mut context = ProjectContext {
            cwd: PathBuf::from("/project"),
            project_root: None,
            workspace_root: None,
            targets: Vec::new(),
            markers: Vec::new(),
        };

        include_instance_targets(&Registry::standard(), &[instance], &mut context).unwrap();
        assert_eq!(context.targets.len(), 1);
        assert_eq!(
            context.targets[0].id,
            format!("node-global:{}", prefix.display())
        );
    }
}
