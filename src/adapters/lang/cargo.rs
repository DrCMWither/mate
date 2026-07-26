use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use super::crates_io;
use crate::adapters::{validate_query, ManagerAdapter};
use crate::context::ProjectContext;
use crate::model::{
    Candidate, CommandSpec, InstanceScope, ManagerInstance, ManagerKind, MatchKind, Target,
    TargetKind,
};
use crate::platform;
use crate::process::{find_all_on_path, probe_version_sanitized_in, safe_user_executable};

const CRATES_IO_REGISTRY: &str = "crates-io";
const CARGO_ENV_REMOVALS: &[&str] = &["CARGO_", "RUSTC", "RUSTDOC", "RUSTFLAGS", "RUSTUP_"];

pub struct CargoAdapter;

#[async_trait]
impl ManagerAdapter for CargoAdapter {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Cargo
    }

    fn supports_fuzzy_fallback(&self) -> bool {
        true
    }

    async fn discover(&self, context: &ProjectContext) -> Result<Vec<ManagerInstance>> {
        let probe_cwd = platform::home_dir().unwrap_or_else(|| context.cwd.clone());
        let mut instances = Vec::new();
        for executable in find_all_on_path("cargo")
            .into_iter()
            .filter(|path| safe_user_executable(path, context.workspace_root.as_deref()))
        {
            let version = probe_version_sanitized_in(
                &executable,
                &["--version"],
                CARGO_ENV_REMOVALS,
                &[("CARGO_TERM_COLOR", "never")],
                Some(&probe_cwd),
            )
            .await;
            instances.push(ManagerInstance {
                id: format!("cargo:{}", executable.display()),
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
        Ok(crates_io::search(query)
            .await?
            .into_iter()
            .map(|package| {
                let match_name = package.name;
                Candidate {
                    query: query.to_owned(),
                    package: match_name.clone(),
                    match_name,
                    manager_instance_id: instance.id.clone(),
                    manager: ManagerKind::Cargo,
                    source: "https://crates.io".into(),
                    version: Some(package.version),
                    description: package.description,
                    score: 0,
                    match_kind: MatchKind::None,
                    verified: true,
                }
            })
            .collect())
    }

    fn compatible_targets(
        &self,
        _instance: &ManagerInstance,
        context: &ProjectContext,
    ) -> Vec<Target> {
        context
            .targets
            .iter()
            .filter(|target| target.kind == TargetKind::CargoRoot)
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
        if target.kind != TargetKind::CargoRoot {
            return Err(anyhow!("cargo only supports a Cargo install root target"));
        }
        let root = target
            .path
            .as_ref()
            .ok_or_else(|| anyhow!("Cargo install root target has no path"))?;
        let root_arg = root
            .to_str()
            .ok_or_else(|| anyhow!("Cargo install root is not valid UTF-8"))?;
        let cargo_home = root.join(".mate-cache").join("cargo-home");
        let cargo_home_arg = cargo_home
            .to_str()
            .ok_or_else(|| anyhow!("Cargo cache path is not valid UTF-8"))?;
        let cwd = platform::home_dir().unwrap_or_else(|| context.cwd.clone());

        packages
            .iter()
            .enumerate()
            .map(|(index, package)| {
                validate_exact_spec(package)?;
                let args = instance.prepend_launcher(vec![
                    "--config".into(),
                    "net.git-fetch-with-cli=false".into(),
                    "install".into(),
                    "--locked".into(),
                    "--root".into(),
                    root_arg.to_owned(),
                    "--registry".into(),
                    CRATES_IO_REGISTRY.into(),
                    "--color".into(),
                    "never".into(),
                    package.clone(),
                ]);
                Ok(CommandSpec {
                    label: format!("cargo install {package}"),
                    program: instance.executable.clone(),
                    args,
                    cwd: Some(cwd.clone()),
                    env: BTreeMap::from([
                        ("CARGO_HOME".into(), cargo_home_arg.to_owned()),
                        ("CARGO_TERM_COLOR".into(), "never".into()),
                        ("RUSTC_WRAPPER".into(), String::new()),
                        ("RUSTC_WORKSPACE_WRAPPER".into(), String::new()),
                    ]),
                    env_remove_prefixes: CARGO_ENV_REMOVALS
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect(),
                    must_not_exist: (!target.exists && index == 0).then(|| root.clone()),
                    requires_admin: false,
                })
            })
            .collect()
    }
}

fn validate_exact_spec(spec: &str) -> Result<()> {
    let (name, version) = spec
        .split_once('@')
        .ok_or_else(|| anyhow!("Cargo package {spec:?} is not pinned to an exact version"))?;
    if !crates_io::valid_crate_name(name) {
        return Err(anyhow!("invalid Cargo crate name {name:?}"));
    }
    semver::Version::parse(version).map_err(|_| anyhow!("invalid Cargo version {version:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{validate_exact_spec, CargoAdapter};
    use crate::adapters::ManagerAdapter;
    use crate::context::ProjectContext;
    use crate::model::{InstanceScope, ManagerInstance, ManagerKind, Target, TargetKind};

    #[test]
    fn accepts_only_exact_safe_crate_specs() {
        assert!(validate_exact_spec("ripgrep@14.1.1").is_ok());
        assert!(validate_exact_spec("tool@1.0.0-alpha.1+build").is_ok());
        assert!(validate_exact_spec("ripgrep").is_err());
        assert!(validate_exact_spec("--config@1.0.0").is_err());
        assert!(validate_exact_spec("bad@name@1.0.0").is_err());
    }

    #[test]
    fn plans_separate_locked_steps_and_one_creation_guard() {
        let adapter = CargoAdapter;
        let instance = ManagerInstance {
            id: "cargo:test".into(),
            kind: ManagerKind::Cargo,
            executable: PathBuf::from("/usr/bin/cargo"),
            launcher_args: Vec::new(),
            version: "cargo 1.90.0".into(),
            scope: InstanceScope::Generic,
        };
        let target = Target {
            id: "cargo-root:/project/.mate/cargo".into(),
            kind: TargetKind::CargoRoot,
            label: "project Cargo root".into(),
            path: Some(PathBuf::from("/project/.mate/cargo")),
            exists: false,
        };
        let context = ProjectContext {
            cwd: PathBuf::from("/project"),
            project_root: Some(PathBuf::from("/project")),
            workspace_root: Some(PathBuf::from("/project")),
            targets: vec![target.clone()],
            markers: vec!["Cargo.toml".into()],
        };
        let steps = adapter
            .plan(
                &instance,
                &["ripgrep@14.1.1".into(), "cargo-edit@0.13.7".into()],
                &target,
                &context,
            )
            .unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps[0].must_not_exist,
            Some(PathBuf::from("/project/.mate/cargo"))
        );
        assert_eq!(steps[1].must_not_exist, None);
        assert!(steps[0].args.iter().any(|arg| arg == "--locked"));
        assert!(steps[0].args.iter().any(|arg| arg == "--registry"));
        assert!(!steps[0].args.iter().any(|arg| arg == "--force"));
        assert!(!steps[0].args.iter().any(|arg| arg == "--no-track"));
    }
}
