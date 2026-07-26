use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Result};

use crate::adapters::{validate_package_id, Registry};
use crate::context::ProjectContext;
use crate::model::{InstallPlan, ManagerInstance, ManagerKind, Selection};

pub fn build_plan(
    registry: &Registry,
    instances: &[ManagerInstance],
    selections: Vec<Selection>,
    context: &ProjectContext,
) -> Result<InstallPlan> {
    validate_new_target_ownership(&selections)?;

    let mut groups: BTreeMap<(String, String), Vec<Selection>> = BTreeMap::new();
    for selection in selections.iter().cloned() {
        groups
            .entry((
                selection.candidate.manager_instance_id.clone(),
                selection.target.id.clone(),
            ))
            .or_default()
            .push(selection);
    }

    let mut steps = Vec::new();
    for ((instance_id, _target_id), group) in groups {
        let instance = instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .ok_or_else(|| anyhow!("manager instance {instance_id} disappeared"))?;
        let adapter = registry.adapter(instance.kind)?;
        let target = &group[0].target;
        let mut seen_packages = BTreeSet::new();
        let packages = group
            .iter()
            .map(resolved_spec)
            .filter(|package| seen_packages.insert(package.clone()))
            .collect::<Vec<_>>();
        for package in &packages {
            validate_package_id(package)?;
        }
        steps.extend(adapter.plan(instance, &packages, target, context)?);
    }

    Ok(InstallPlan { selections, steps })
}

fn validate_new_target_ownership(selections: &[Selection]) -> Result<()> {
    let mut owners = BTreeMap::<String, String>::new();
    for selection in selections
        .iter()
        .filter(|selection| !selection.target.exists)
    {
        let instance_id = &selection.candidate.manager_instance_id;
        if let Some(previous) = owners.insert(selection.target.id.clone(), instance_id.clone()) {
            if previous != *instance_id {
                return Err(anyhow!(
                    "new target {} cannot be created by multiple manager instances ({previous} and {instance_id}) in one plan",
                    selection.target.id
                ));
            }
        }
    }
    Ok(())
}

fn resolved_spec(selection: &Selection) -> String {
    match (
        selection.candidate.manager,
        selection.candidate.version.as_deref(),
    ) {
        (ManagerKind::Pip | ManagerKind::Uv, Some(version)) => {
            format!("{}=={version}", selection.candidate.package)
        }
        (ManagerKind::Cargo | ManagerKind::Npm, Some(version)) => {
            format!("{}@{version}", selection.candidate.package)
        }
        _ => selection.candidate.package.clone(),
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{Candidate, ManagerKind, MatchKind, Selection, Target, TargetKind};

    use super::{resolved_spec, validate_new_target_ownership};

    #[test]
    fn pins_python_candidates_to_searched_version() {
        let selection = Selection {
            query: "requests".into(),
            candidate: Candidate {
                query: "requests".into(),
                package: "requests".into(),
                match_name: "requests".into(),
                manager_instance_id: "uv:test".into(),
                manager: ManagerKind::Uv,
                source: "https://pypi.org/simple".into(),
                version: Some("2.32.4".into()),
                description: None,
                score: 1_000,
                match_kind: MatchKind::Exact,
                verified: true,
            },
            target: Target {
                id: "user".into(),
                kind: TargetKind::User,
                label: "user".into(),
                path: None,
                exists: true,
            },
        };
        assert_eq!(resolved_spec(&selection), "requests==2.32.4");
    }

    #[test]
    fn pins_cargo_and_npm_candidates_with_at_syntax() {
        let make_selection = |manager, package: &str, version: &str| Selection {
            query: package.into(),
            candidate: Candidate {
                query: package.into(),
                package: package.into(),
                match_name: package.into(),
                manager_instance_id: format!("{manager}:test"),
                manager,
                source: "registry".into(),
                version: Some(version.into()),
                description: None,
                score: 1_000,
                match_kind: MatchKind::Exact,
                verified: true,
            },
            target: Target {
                id: "target".into(),
                kind: TargetKind::User,
                label: "target".into(),
                path: None,
                exists: true,
            },
        };

        assert_eq!(
            resolved_spec(&make_selection(ManagerKind::Cargo, "ripgrep", "14.1.1")),
            "ripgrep@14.1.1"
        );
        assert_eq!(
            resolved_spec(&make_selection(ManagerKind::Npm, "typescript", "5.8.3")),
            "typescript@5.8.3"
        );
    }

    #[test]
    fn rejects_two_uv_instances_creating_the_same_venv() {
        let target = Target {
            id: "venv:/project/.venv".into(),
            kind: TargetKind::PythonVenv,
            label: "new venv".into(),
            path: Some("/project/.venv".into()),
            exists: false,
        };
        let make_selection = |instance: &str, package: &str| Selection {
            query: package.into(),
            candidate: Candidate {
                query: package.into(),
                package: package.into(),
                match_name: package.into(),
                manager_instance_id: instance.into(),
                manager: ManagerKind::Uv,
                source: "https://pypi.org/simple".into(),
                version: Some("1.0.0".into()),
                description: None,
                score: 1_000,
                match_kind: MatchKind::Exact,
                verified: true,
            },
            target: target.clone(),
        };
        let selections = vec![
            make_selection("uv:/one", "alpha"),
            make_selection("uv:/two", "beta"),
        ];
        assert!(validate_new_target_ownership(&selections).is_err());
    }
}
