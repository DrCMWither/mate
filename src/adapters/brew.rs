use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use super::{score_match, validate_query, ManagerAdapter};
use crate::context::ProjectContext;
use crate::model::{
    Candidate, CommandSpec, InstanceScope, ManagerInstance, ManagerKind, Target, TargetKind,
};
use crate::process::{
    find_all_on_path, probe_version, safe_diagnostic, safe_user_executable,
    search_command_sanitized,
};

pub struct BrewAdapter;

#[async_trait]
impl ManagerAdapter for BrewAdapter {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Brew
    }

    async fn discover(&self, context: &ProjectContext) -> Result<Vec<ManagerInstance>> {
        let mut instances = Vec::new();
        for executable in find_all_on_path("brew")
            .into_iter()
            .filter(|path| safe_user_executable(path, context.workspace_root.as_deref()))
        {
            let version = probe_version(&executable, &["--version"]).await;
            instances.push(ManagerInstance {
                id: format!("brew:{}", executable.display()),
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
        let output = search_command_sanitized(
            &instance.executable,
            &["search", "--formula", query],
            &["HOMEBREW_"],
            &[
                ("HOMEBREW_NO_AUTO_UPDATE", "1"),
                ("HOMEBREW_NO_ANALYTICS", "1"),
            ],
        )
        .await?;
        if !output.success {
            return Err(anyhow!("{}", safe_diagnostic(output.stderr.trim())));
        }
        Ok(parse_search(&output.stdout, query, &instance.id))
    }

    fn compatible_targets(
        &self,
        _instance: &ManagerInstance,
        context: &ProjectContext,
    ) -> Vec<Target> {
        context
            .targets
            .iter()
            .filter(|target| target.kind == TargetKind::User)
            .cloned()
            .collect()
    }

    fn plan(
        &self,
        instance: &ManagerInstance,
        packages: &[String],
        target: &Target,
        _context: &ProjectContext,
    ) -> Result<Vec<CommandSpec>> {
        if target.kind != TargetKind::User {
            return Err(anyhow!("brew only supports the user target"));
        }
        let mut args = vec!["install".into(), "--formula".into()];
        args.extend(packages.iter().cloned());
        let env = BTreeMap::from([
            ("HOMEBREW_NO_AUTO_UPDATE".into(), "1".into()),
            ("HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK".into(), "1".into()),
            ("HOMEBREW_NO_INSTALL_CLEANUP".into(), "1".into()),
        ]);
        Ok(vec![CommandSpec {
            label: format!("brew install {} package(s)", packages.len()),
            program: instance.executable.clone(),
            args,
            cwd: None,
            env,
            env_remove_prefixes: vec!["HOMEBREW_".into()],
            must_not_exist: None,
            requires_admin: false,
        }])
    }
}

pub(crate) fn parse_search(text: &str, query: &str, instance_id: &str) -> Vec<Candidate> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("==>"))
        .flat_map(|line| line.split_whitespace())
        .map(|package| Candidate {
            query: query.to_owned(),
            package: package.to_owned(),
            manager_instance_id: instance_id.to_owned(),
            manager: ManagerKind::Brew,
            source: "Homebrew formulae".into(),
            version: None,
            description: None,
            score: score_match(query, package),
            verified: true,
        })
        .take(40)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_search;

    #[test]
    fn ignores_brew_section_headers() {
        let found = parse_search(
            "==> Formulae\nripgrep\n\n==> Casks\nripgrep-app\n",
            "ripgrep",
            "brew:test",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].package, "ripgrep");
    }
}
