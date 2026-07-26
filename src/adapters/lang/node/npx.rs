use anyhow::{Error, Result};
use async_trait::async_trait;

use super::{discover_launchers, NODE_ENV_REMOVALS};
use crate::adapters::ManagerAdapter;
use crate::context::ProjectContext;
use crate::model::{Candidate, CommandSpec, InstanceScope, ManagerInstance, ManagerKind, Target};
use crate::process::probe_version_sanitized;

const NPX_INSTALL_ERROR: &str =
    "npx executes package binaries and is not a persistent installer; use npm install for packages (a future mate exec command can expose npx safely)";

pub struct NpxAdapter;

#[async_trait]
impl ManagerAdapter for NpxAdapter {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Npx
    }

    fn supports_search(&self) -> bool {
        false
    }

    fn supports_install(&self) -> bool {
        false
    }

    async fn discover(&self, context: &ProjectContext) -> Result<Vec<ManagerInstance>> {
        let mut instances = Vec::new();
        for launcher in discover_launchers("npx", "npx-cli.js", context) {
            let version_args = launcher.prepend_launcher(vec!["--version".into()]);
            let version = probe_version_sanitized(
                &launcher.program,
                &version_args,
                NODE_ENV_REMOVALS,
                &[("NPM_CONFIG_COLOR", "false")],
            )
            .await;
            instances.push(ManagerInstance {
                id: launcher.instance_id("npx"),
                kind: self.kind(),
                executable: launcher.program,
                launcher_args: launcher.launcher_args,
                version,
                scope: InstanceScope::Generic,
            });
        }
        Ok(instances)
    }

    async fn search(&self, _instance: &ManagerInstance, _query: &str) -> Result<Vec<Candidate>> {
        Err(Error::msg(NPX_INSTALL_ERROR))
    }

    fn compatible_targets(
        &self,
        _instance: &ManagerInstance,
        _context: &ProjectContext,
    ) -> Vec<Target> {
        Vec::new()
    }

    fn plan(
        &self,
        _instance: &ManagerInstance,
        _packages: &[String],
        _target: &Target,
        _context: &ProjectContext,
    ) -> Result<Vec<CommandSpec>> {
        Err(Error::msg(NPX_INSTALL_ERROR))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::NpxAdapter;
    use crate::adapters::ManagerAdapter;
    use crate::context::ProjectContext;
    use crate::model::{InstanceScope, ManagerInstance, ManagerKind, Target, TargetKind};

    fn context() -> ProjectContext {
        ProjectContext {
            cwd: PathBuf::from("project"),
            project_root: None,
            workspace_root: None,
            targets: Vec::new(),
            markers: Vec::new(),
        }
    }

    fn instance() -> ManagerInstance {
        ManagerInstance {
            id: "npx:test".into(),
            kind: ManagerKind::Npx,
            executable: PathBuf::from("npx"),
            launcher_args: Vec::new(),
            version: "11.9.0".into(),
            scope: InstanceScope::Generic,
        }
    }

    #[tokio::test]
    async fn refuses_package_search() {
        assert!(!NpxAdapter.supports_search());
        assert!(!NpxAdapter.supports_install());
        let error = NpxAdapter
            .search(&instance(), "typescript")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not a persistent installer"));
    }

    #[test]
    fn refuses_install_plans() {
        let target = Target {
            id: "user".into(),
            kind: TargetKind::User,
            label: "user".into(),
            path: None,
            exists: true,
        };
        let error = NpxAdapter
            .plan(
                &instance(),
                &["typescript@5.9.2".into()],
                &target,
                &context(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("future mate exec"));
    }
}
