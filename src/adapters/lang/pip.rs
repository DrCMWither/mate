use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use super::pypi;
use crate::adapters::{validate_query, ManagerAdapter};
use crate::context::ProjectContext;
use crate::model::{
    Candidate, CommandSpec, InstanceScope, ManagerInstance, ManagerKind, Target, TargetKind,
};
use crate::platform;
use crate::process::{find_all_on_path, probe_version, safe_user_executable};

pub struct PipAdapter;

#[async_trait]
impl ManagerAdapter for PipAdapter {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Pip
    }

    async fn discover(&self, context: &ProjectContext) -> Result<Vec<ManagerInstance>> {
        let mut paths = BTreeSet::new();
        for name in ["pip", "pip3"] {
            paths.extend(
                find_all_on_path(name)
                    .into_iter()
                    .filter(|path| safe_user_executable(path, context.workspace_root.as_deref())),
            );
        }

        let mut instances = Vec::new();
        for executable in paths {
            let scope = context
                .targets
                .iter()
                .filter(|target| target.kind == TargetKind::PythonVenv && target.exists)
                .filter_map(|target| target.path.as_ref())
                .find(|venv| executable.starts_with(venv))
                .cloned()
                .map(InstanceScope::PythonVenv)
                .unwrap_or(InstanceScope::Generic);
            let version = probe_version(&executable, &["--version"]).await;
            instances.push(ManagerInstance {
                id: format!("pip:{}", executable.display()),
                kind: self.kind(),
                executable,
                launcher_args: Vec::new(),
                version,
                scope,
            });
        }
        Ok(instances)
    }

    async fn search(&self, instance: &ManagerInstance, query: &str) -> Result<Vec<Candidate>> {
        validate_query(query)?;
        let Some(package) = pypi::exact_lookup(query).await? else {
            return Ok(Vec::new());
        };
        Ok(vec![Candidate {
            query: query.to_owned(),
            package: package.name,
            manager_instance_id: instance.id.clone(),
            manager: self.kind(),
            source: "https://pypi.org/simple".into(),
            version: Some(package.version),
            description: package.summary,
            score: 100,
            verified: true,
        }])
    }

    fn compatible_targets(
        &self,
        instance: &ManagerInstance,
        context: &ProjectContext,
    ) -> Vec<Target> {
        match &instance.scope {
            InstanceScope::PythonVenv(path) => context
                .targets
                .iter()
                .filter(|target| {
                    target.kind == TargetKind::PythonVenv
                        && target.exists
                        && target.path.as_ref() == Some(path)
                })
                .cloned()
                .collect(),
            InstanceScope::Generic => context
                .targets
                .iter()
                .filter(|target| target.kind == TargetKind::User)
                .cloned()
                .collect(),
            InstanceScope::NodeGlobal(_) => Vec::new(),
        }
    }

    fn plan(
        &self,
        instance: &ManagerInstance,
        packages: &[String],
        target: &Target,
        _context: &ProjectContext,
    ) -> Result<Vec<CommandSpec>> {
        let mut args = vec![
            "--disable-pip-version-check".into(),
            "install".into(),
            "--index-url".into(),
            "https://pypi.org/simple".into(),
        ];
        match (&instance.scope, target.kind) {
            (InstanceScope::Generic, TargetKind::User) => args.push("--user".into()),
            (InstanceScope::PythonVenv(path), TargetKind::PythonVenv)
                if target.exists && target.path.as_ref() == Some(path) => {}
            _ => {
                return Err(anyhow!(
                    "pip instance {} is incompatible with target {}",
                    instance.executable.display(),
                    target.id
                ));
            }
        }
        args.extend(packages.iter().cloned());

        let env = BTreeMap::from([("PIP_CONFIG_FILE".into(), platform::null_device().into())]);
        Ok(vec![CommandSpec {
            label: format!("pip install {} package(s)", packages.len()),
            program: instance.executable.clone(),
            args,
            cwd: None,
            env,
            env_remove_prefixes: vec!["PIP_".into(), "PYTHON".into(), "VIRTUAL_ENV".into()],
            must_not_exist: None,
            requires_admin: false,
        }])
    }
}
