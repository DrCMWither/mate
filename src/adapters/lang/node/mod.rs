mod npm;
mod npx;

use std::path::PathBuf;
use std::sync::Arc;

use crate::adapters::ManagerAdapter;
use crate::context::ProjectContext;
#[cfg(windows)]
use crate::platform;
use crate::process::{find_all_on_path, safe_user_executable};

pub(super) const NODE_ENV_REMOVALS: &[&str] =
    &["NODE_OPTIONS", "NODE_PATH", "NPM_", "npm_", "INIT_CWD"];

#[derive(Debug, Clone)]
pub(super) struct NodeLauncher {
    pub program: PathBuf,
    pub launcher_args: Vec<String>,
}

impl NodeLauncher {
    pub fn prepend_launcher(&self, args: Vec<String>) -> Vec<String> {
        let mut invocation = Vec::with_capacity(self.launcher_args.len() + args.len());
        invocation.extend(self.launcher_args.iter().cloned());
        invocation.extend(args);
        invocation
    }

    pub fn instance_id(&self, manager: &str) -> String {
        format!("{manager}:{}", self.program.display())
    }
}

pub(super) fn standard_adapters() -> Vec<Arc<dyn ManagerAdapter>> {
    vec![Arc::new(npm::NpmAdapter), Arc::new(npx::NpxAdapter)]
}

pub(super) fn discover_launchers(
    command: &str,
    windows_script: &str,
    context: &ProjectContext,
) -> Vec<NodeLauncher> {
    #[cfg(windows)]
    {
        let _ = command;
        let mut launchers = Vec::new();
        for program in find_all_on_path("node")
            .into_iter()
            .filter(|path| safe_user_executable(path, context.workspace_root.as_deref()))
        {
            let Some(root) = program.parent() else {
                continue;
            };
            let script = root
                .join("node_modules")
                .join("npm")
                .join("bin")
                .join(windows_script);
            let Ok(script) = platform::canonicalize(script) else {
                continue;
            };
            if script.is_file() && script.starts_with(root) {
                launchers.push(NodeLauncher {
                    program,
                    launcher_args: vec![script.to_string_lossy().into_owned()],
                });
            }
        }
        launchers
    }

    #[cfg(not(windows))]
    {
        let _ = windows_script;
        find_all_on_path(command)
            .into_iter()
            .filter(|path| safe_user_executable(path, context.workspace_root.as_deref()))
            .map(|program| NodeLauncher {
                program,
                launcher_args: Vec::new(),
            })
            .collect()
    }
}

pub(super) fn owned_env_removals() -> Vec<String> {
    NODE_ENV_REMOVALS
        .iter()
        .map(|value| (*value).to_owned())
        .collect()
}
