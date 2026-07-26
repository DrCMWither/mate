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
    find_trusted_system_executable, probe_version, safe_diagnostic, search_command,
};

pub struct PacmanAdapter;

#[async_trait]
impl ManagerAdapter for PacmanAdapter {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Pacman
    }

    fn supports_fuzzy_fallback(&self) -> bool {
        true
    }

    async fn discover(&self, _context: &ProjectContext) -> Result<Vec<ManagerInstance>> {
        let Some(executable) = find_trusted_system_executable("pacman") else {
            return Ok(Vec::new());
        };
        let version = probe_version(&executable, &["--version"]).await;
        Ok(vec![ManagerInstance {
            id: format!("pacman:{}", executable.display()),
            kind: self.kind(),
            executable,
            launcher_args: Vec::new(),
            version,
            scope: InstanceScope::Generic,
        }])
    }

    async fn search(&self, instance: &ManagerInstance, query: &str) -> Result<Vec<Candidate>> {
        validate_query(query)?;
        let output =
            search_command(&instance.executable, &["-Ss", "--color", "never", query]).await?;
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
            return Err(anyhow!("pacman only supports the system target"));
        }
        let mut args = vec!["-S".into(), "--needed".into(), "--".into()];
        args.extend(packages.iter().cloned());
        Ok(vec![CommandSpec {
            label: format!("pacman install {} package(s)", packages.len()),
            program: instance.executable.clone(),
            args,
            cwd: None,
            env: BTreeMap::new(),
            env_remove_prefixes: Vec::new(),
            must_not_exist: None,
            requires_admin: true,
        }])
    }
}

pub(crate) fn parse_search(text: &str, query: &str, instance_id: &str) -> Vec<Candidate> {
    let mut found = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(header) = lines.next() {
        if header.starts_with(char::is_whitespace) || header.trim().is_empty() {
            continue;
        }
        let mut fields = header.split_whitespace();
        let Some(repository_and_name) = fields.next() else {
            continue;
        };
        let Some((repository, package)) = repository_and_name.split_once('/') else {
            continue;
        };
        let version = fields.next().map(str::to_owned);
        let description = lines
            .peek()
            .filter(|line| line.starts_with(char::is_whitespace))
            .map(|line| line.trim().to_owned());
        if description.is_some() {
            lines.next();
        }
        found.push(Candidate {
            query: query.to_owned(),
            package: format!("{repository}/{package}"),
            match_name: package.to_owned(),
            manager_instance_id: instance_id.to_owned(),
            manager: ManagerKind::Pacman,
            source: format!("pacman repository {repository}"),
            version,
            description,
            score: 0,
            match_kind: MatchKind::None,
            verified: true,
        });
        if found.len() >= MAX_RAW_CANDIDATES {
            break;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::parse_search;

    #[test]
    fn parses_pacman_search_pairs() {
        let found = parse_search(
            "extra/ripgrep 14.1.1-1\n    A search tool\nextra/ripgrep-all 0.10.6-1\n    Search PDFs\n",
            "ripgrep",
            "pacman:test",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].package, "extra/ripgrep");
        assert_eq!(found[0].match_name, "ripgrep");
        assert_eq!(found[0].version.as_deref(), Some("14.1.1-1"));
        assert_eq!(found[0].description.as_deref(), Some("A search tool"));
    }
}
