use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use super::pypi;
use crate::adapters::{validate_query, ManagerAdapter};
use crate::context::{python_environment_python, venv_python, ProjectContext};
use crate::model::{
    Candidate, CommandSpec, InstanceScope, ManagerInstance, ManagerKind, MatchKind, Target,
    TargetKind,
};
use crate::process::{find_all_on_path, probe_version, safe_user_executable};

pub struct UvAdapter;

#[async_trait]
impl ManagerAdapter for UvAdapter {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Uv
    }

    async fn discover(&self, context: &ProjectContext) -> Result<Vec<ManagerInstance>> {
        let mut instances = Vec::new();
        for executable in find_all_on_path("uv")
            .into_iter()
            .filter(|path| safe_user_executable(path, context.workspace_root.as_deref()))
        {
            let version = probe_version(&executable, &["--version"]).await;
            instances.push(ManagerInstance {
                id: format!("uv:{}", executable.display()),
                kind: self.kind(),
                executable,
                launcher_args: Vec::new(),
                version,
                scope: InstanceScope::Generic,
            });
        }
        Ok(instances)
    }

    async fn search(&self, instance: &ManagerInstance, query: &str) -> Result<Vec<Candidate>> {
        validate_query(query)?;
        let Some(package) = pypi::exact_lookup(query).await? else {
            return Ok(Vec::new());
        };
        let match_name = package.name;
        Ok(vec![Candidate {
            query: query.to_owned(),
            package: match_name.clone(),
            match_name,
            manager_instance_id: instance.id.clone(),
            manager: self.kind(),
            source: "https://pypi.org/simple".into(),
            version: Some(package.version),
            description: package.summary,
            score: 0,
            match_kind: MatchKind::None,
            verified: true,
        }])
    }

    fn compatible_targets(
        &self,
        _instance: &ManagerInstance,
        context: &ProjectContext,
    ) -> Vec<Target> {
        context
            .targets
            .iter()
            .filter(|target| matches!(target.kind, TargetKind::User | TargetKind::PythonVenv))
            .cloned()
            .collect()
    }

    fn plan(
        &self,
        instance: &ManagerInstance,
        packages: &[String],
        target: &Target,
        context: &ProjectContext,
    ) -> Result<Vec<CommandSpec>> {
        match target.kind {
            TargetKind::User => Ok(packages
                .iter()
                .map(|package| CommandSpec {
                    label: format!("uv tool install {package}"),
                    program: instance.executable.clone(),
                    args: vec![
                        "tool".into(),
                        "install".into(),
                        "--no-config".into(),
                        "--default-index".into(),
                        "https://pypi.org/simple".into(),
                        package.clone(),
                    ],
                    cwd: Some(context.cwd.clone()),
                    env: BTreeMap::new(),
                    env_remove_prefixes: python_env_removals(),
                    must_not_exist: None,
                    requires_admin: false,
                })
                .collect()),
            TargetKind::PythonVenv => {
                let path = target
                    .path
                    .as_ref()
                    .ok_or_else(|| anyhow!("Python venv target has no path"))?;
                let mut steps = Vec::new();
                if !target.exists {
                    steps.push(CommandSpec {
                        label: format!("create Python venv {}", path.display()),
                        program: instance.executable.clone(),
                        args: vec![
                            "venv".into(),
                            "--no-config".into(),
                            path.to_string_lossy().into_owned(),
                        ],
                        cwd: Some(context.cwd.clone()),
                        env: BTreeMap::new(),
                        env_remove_prefixes: python_env_removals(),
                        must_not_exist: Some(path.clone()),
                        requires_admin: false,
                    });
                }

                let mut args = vec![
                    "pip".into(),
                    "install".into(),
                    "--no-config".into(),
                    "--default-index".into(),
                    "https://pypi.org/simple".into(),
                    "--python".into(),
                    python_environment_python(path)
                        .unwrap_or_else(|| venv_python(path))
                        .to_string_lossy()
                        .into_owned(),
                ];
                args.extend(packages.iter().cloned());
                steps.push(CommandSpec {
                    label: format!("uv install {} package(s) into venv", packages.len()),
                    program: instance.executable.clone(),
                    args,
                    cwd: Some(context.cwd.clone()),
                    env: BTreeMap::new(),
                    env_remove_prefixes: python_env_removals(),
                    must_not_exist: None,
                    requires_admin: false,
                });
                Ok(steps)
            }
            _ => Err(anyhow!("uv is incompatible with target {}", target.id)),
        }
    }
}

fn python_env_removals() -> Vec<String> {
    vec![
        "UV_".into(),
        "PIP_".into(),
        "PYTHON".into(),
        "VIRTUAL_ENV".into(),
    ]
}
