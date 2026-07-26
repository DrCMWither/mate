use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use super::{validate_query, ManagerAdapter, MAX_RAW_CANDIDATES};
use crate::context::ProjectContext;
use crate::model::{
    Candidate, CommandSpec, InstanceScope, ManagerInstance, ManagerKind, MatchKind, Target,
    TargetKind,
};
use crate::process::{
    find_trusted_system_executable, probe_version, safe_diagnostic, search_command_sanitized,
};

pub struct AptAdapter;

#[async_trait]
impl ManagerAdapter for AptAdapter {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Apt
    }

    fn supports_fuzzy_fallback(&self) -> bool {
        true
    }

    async fn discover(&self, _context: &ProjectContext) -> Result<Vec<ManagerInstance>> {
        let Some(executable) = find_trusted_system_executable("apt-get") else {
            return Ok(Vec::new());
        };
        let version = probe_version(&executable, &["--version"]).await;
        Ok(vec![ManagerInstance {
            id: format!("apt:{}", executable.display()),
            kind: self.kind(),
            executable,
            launcher_args: Vec::new(),
            version,
            scope: InstanceScope::Generic,
        }])
    }

    async fn search(&self, instance: &ManagerInstance, query: &str) -> Result<Vec<Candidate>> {
        validate_query(query)?;
        let apt_cache = find_trusted_system_executable("apt-cache")
            .ok_or_else(|| anyhow!("trusted system apt-cache is not available"))?;
        let output = search_command_sanitized(
            &apt_cache,
            &["search", "--names-only", query],
            &["APT_CONFIG"],
            &[],
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
            .filter(|target| target.kind == TargetKind::System)
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
        if target.kind != TargetKind::System {
            return Err(anyhow!("apt only supports the system target"));
        }
        let mut args = vec!["install".into(), "--".into()];
        args.extend(packages.iter().cloned());
        Ok(vec![CommandSpec {
            label: format!("apt install {} package(s)", packages.len()),
            program: instance.executable.clone(),
            args,
            cwd: None,
            env: BTreeMap::new(),
            env_remove_prefixes: vec!["APT_CONFIG".into()],
            must_not_exist: None,
            requires_admin: true,
        }])
    }
}

pub(crate) fn parse_search(text: &str, query: &str, instance_id: &str) -> Vec<Candidate> {
    text.lines()
        .filter_map(|line| {
            let (package, description) = line.split_once(" - ")?;
            let package = package.trim();
            if package.is_empty() {
                return None;
            }
            Some(Candidate {
                query: query.to_owned(),
                package: package.to_owned(),
                match_name: package.to_owned(),
                manager_instance_id: instance_id.to_owned(),
                manager: ManagerKind::Apt,
                source: "APT configured repositories".into(),
                version: None,
                description: Some(description.trim().to_owned()).filter(|s| !s.is_empty()),
                score: 0,
                match_kind: MatchKind::None,
                verified: true,
            })
        })
        .take(MAX_RAW_CANDIDATES)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_search;

    #[test]
    fn parses_apt_cache_lines() {
        let found = parse_search(
            "ripgrep - recursively searches directories\nelpa-rg - Emacs search UI\n",
            "ripgrep",
            "apt:test",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].package, "ripgrep");
        assert_eq!(found[0].match_name, "ripgrep");
    }
}
