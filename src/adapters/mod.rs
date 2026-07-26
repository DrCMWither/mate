mod apt;
mod brew;
mod lang;
mod pacman;

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::{future::join_all, stream, StreamExt};

use crate::context::ProjectContext;
use crate::matching;
use crate::model::{Candidate, CommandSpec, ManagerInstance, ManagerKind, MatchKind, Target};

const MAX_RAW_CANDIDATES: usize = 256;
const MAX_RANKED_CANDIDATES: usize = 40;

#[async_trait]
pub trait ManagerAdapter: Send + Sync {
    fn kind(&self) -> ManagerKind;

    fn supports_search(&self) -> bool {
        true
    }

    fn supports_install(&self) -> bool {
        true
    }

    fn supports_fuzzy_fallback(&self) -> bool {
        false
    }

    async fn discover(&self, context: &ProjectContext) -> Result<Vec<ManagerInstance>>;

    async fn search(&self, instance: &ManagerInstance, query: &str) -> Result<Vec<Candidate>>;

    fn compatible_targets(
        &self,
        instance: &ManagerInstance,
        context: &ProjectContext,
    ) -> Vec<Target>;

    fn plan(
        &self,
        instance: &ManagerInstance,
        packages: &[String],
        target: &Target,
        context: &ProjectContext,
    ) -> Result<Vec<CommandSpec>>;
}

pub struct Registry {
    adapters: Vec<Arc<dyn ManagerAdapter>>,
}

impl Registry {
    pub fn standard() -> Self {
        let mut adapters: Vec<Arc<dyn ManagerAdapter>> = vec![
            Arc::new(apt::AptAdapter),
            Arc::new(pacman::PacmanAdapter),
            Arc::new(brew::BrewAdapter),
        ];
        adapters.extend(lang::standard_adapters());
        Self { adapters }
    }

    pub fn adapter(&self, kind: ManagerKind) -> Result<Arc<dyn ManagerAdapter>> {
        self.adapters
            .iter()
            .find(|adapter| adapter.kind() == kind)
            .cloned()
            .ok_or_else(|| anyhow!("no adapter registered for {kind}"))
    }

    pub fn ensure_searchable(&self, requested: &[ManagerKind]) -> Result<()> {
        self.ensure_capability(requested, |adapter| adapter.supports_search(), "search")
    }

    pub fn ensure_installable(&self, requested: &[ManagerKind]) -> Result<()> {
        self.ensure_capability(requested, |adapter| adapter.supports_install(), "install")
    }

    fn ensure_capability<F>(
        &self,
        requested: &[ManagerKind],
        supported: F,
        operation: &str,
    ) -> Result<()>
    where
        F: Fn(&dyn ManagerAdapter) -> bool,
    {
        let unsupported = requested
            .iter()
            .copied()
            .filter(|kind| {
                self.adapter(*kind)
                    .is_ok_and(|adapter| !supported(adapter.as_ref()))
            })
            .map(|kind| kind.to_string())
            .collect::<Vec<_>>();
        if unsupported.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "{} cannot be used with `mate {operation}`: it executes package binaries instead of persistently installing them; use npm for installation",
                unsupported.join(", ")
            ))
        }
    }

    pub async fn discover(
        &self,
        context: &ProjectContext,
        filter: &[ManagerKind],
    ) -> (Vec<ManagerInstance>, Vec<String>) {
        let selected: Vec<_> = self
            .adapters
            .iter()
            .filter(|adapter| filter.is_empty() || filter.contains(&adapter.kind()))
            .cloned()
            .collect();

        let results = join_all(
            selected
                .into_iter()
                .map(|adapter| async move { (adapter.kind(), adapter.discover(context).await) }),
        )
        .await;

        let mut instances = Vec::new();
        let mut warnings = Vec::new();
        for (kind, result) in results {
            match result {
                Ok(mut found) => instances.append(&mut found),
                Err(error) => warnings.push(format!("{kind}: {error:#}")),
            }
        }
        instances.sort_by(|a, b| a.id.cmp(&b.id));
        (instances, warnings)
    }

    pub async fn search(
        &self,
        instances: &[ManagerInstance],
        queries: &[String],
    ) -> (Vec<Candidate>, Vec<String>) {
        let mut jobs = Vec::new();
        for instance in instances {
            for query in queries {
                let adapter = match self.adapter(instance.kind) {
                    Ok(adapter) => adapter,
                    Err(error) => {
                        return (Vec::new(), vec![error.to_string()]);
                    }
                };
                if !adapter.supports_search() {
                    continue;
                }
                let instance = instance.clone();
                let query = query.clone();
                jobs.push(async move {
                    let outcome = search_with_fallback(adapter, &instance, &query).await;
                    (instance.id, query, outcome)
                });
            }
        }

        let mut candidates = Vec::new();
        let mut warnings = BTreeSet::new();
        let results = stream::iter(jobs)
            .buffer_unordered(8)
            .collect::<Vec<_>>()
            .await;
        for (instance_id, query, mut outcome) in results {
            candidates.append(&mut outcome.candidates);
            for warning in outcome.warnings {
                warnings.insert(format!("{instance_id}, query {query:?}: {warning}"));
            }
        }

        matching::sort_candidates(&mut candidates);
        (candidates, warnings.into_iter().collect())
    }
}

struct SearchOutcome {
    candidates: Vec<Candidate>,
    warnings: Vec<String>,
}

async fn search_with_fallback(
    adapter: Arc<dyn ManagerAdapter>,
    instance: &ManagerInstance,
    query: &str,
) -> SearchOutcome {
    let mut warnings = Vec::new();
    let mut candidates = match adapter.search(instance, query).await {
        Ok(found) => rank_candidates(query, found, true),
        Err(error) => {
            warnings.push(format!("primary search failed: {error:#}"));
            Vec::new()
        }
    };

    let needs_fallback = candidates
        .iter()
        .all(|candidate| candidate.match_kind < MatchKind::Edit);
    if adapter.supports_fuzzy_fallback() && needs_fallback {
        for fallback in matching::fallback_queries(query) {
            match adapter.search(instance, &fallback).await {
                Ok(found) => {
                    let ranked = rank_candidates(query, found, false);
                    let found_identity_match = ranked
                        .iter()
                        .any(|candidate| candidate.match_kind >= MatchKind::CanonicalExact);
                    candidates.extend(ranked);
                    if found_identity_match {
                        break;
                    }
                }
                Err(error) => warnings.push(format!(
                    "fuzzy fallback query {fallback:?} failed: {error:#}"
                )),
            }
        }
    }

    matching::sort_candidates(&mut candidates);
    let mut seen = BTreeSet::new();
    candidates.retain(|candidate| {
        seen.insert((
            candidate.manager,
            candidate.manager_instance_id.clone(),
            candidate.package.to_ascii_lowercase(),
            candidate.version.clone(),
        ))
    });
    candidates.truncate(MAX_RANKED_CANDIDATES);

    SearchOutcome {
        candidates,
        warnings,
    }
}

fn rank_candidates(
    query: &str,
    candidates: Vec<Candidate>,
    preserve_provider_matches: bool,
) -> Vec<Candidate> {
    candidates
        .into_iter()
        .enumerate()
        .filter_map(|(provider_index, mut candidate)| {
            candidate.query = query.to_owned();
            matching::apply(query, &mut candidate);
            if candidate.match_kind == MatchKind::None {
                if !preserve_provider_matches {
                    return None;
                }
                candidate.match_kind = MatchKind::Provider;
                candidate.score = 99_u16.saturating_sub(provider_index.min(49) as u16);
            }
            Some(candidate)
        })
        .collect()
}

pub fn validate_query(query: &str) -> Result<()> {
    const MAX_QUERY_CHARS: usize = 128;

    if query.trim().is_empty() {
        return Err(anyhow!("package query cannot be empty"));
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(anyhow!(
            "package query is limited to {MAX_QUERY_CHARS} characters"
        ));
    }
    if query.starts_with('-') {
        return Err(anyhow!(
            "package query {query:?} looks like a command-line option"
        ));
    }
    if query.chars().any(char::is_control) {
        return Err(anyhow!("package query contains control characters"));
    }
    Ok(())
}

pub fn validate_package_id(package: &str) -> Result<()> {
    if package.trim().is_empty() {
        return Err(anyhow!("resolved package id cannot be empty"));
    }
    if package.starts_with('-') {
        return Err(anyhow!(
            "resolved package id {package:?} looks like a command-line option"
        ));
    }
    if package.chars().any(char::is_control) {
        return Err(anyhow!(
            "resolved package id {package:?} contains control characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use anyhow::{anyhow, Result};
    use async_trait::async_trait;

    use super::{validate_package_id, ManagerAdapter, Registry};
    use crate::context::ProjectContext;
    use crate::model::{
        Candidate, CommandSpec, InstanceScope, ManagerInstance, ManagerKind, MatchKind, Target,
    };

    struct FuzzyFallbackAdapter {
        fail_primary: bool,
    }

    #[async_trait]
    impl ManagerAdapter for FuzzyFallbackAdapter {
        fn kind(&self) -> ManagerKind {
            ManagerKind::Apt
        }

        fn supports_fuzzy_fallback(&self) -> bool {
            true
        }

        async fn discover(&self, _context: &ProjectContext) -> Result<Vec<ManagerInstance>> {
            Ok(Vec::new())
        }

        async fn search(&self, instance: &ManagerInstance, query: &str) -> Result<Vec<Candidate>> {
            if query == "ripgrpe" {
                if self.fail_primary {
                    return Err(anyhow!("simulated primary failure"));
                }
                return Ok(vec![Candidate {
                    query: query.into(),
                    package: "other-search-tool".into(),
                    match_name: "other-search-tool".into(),
                    manager_instance_id: instance.id.clone(),
                    manager: ManagerKind::Apt,
                    source: "test index".into(),
                    version: None,
                    description: Some("ripgrpe compatibility layer".into()),
                    score: 0,
                    match_kind: MatchKind::None,
                    verified: true,
                }]);
            }
            Ok(vec![Candidate {
                query: query.into(),
                package: "ripgrep".into(),
                match_name: "ripgrep".into(),
                manager_instance_id: instance.id.clone(),
                manager: ManagerKind::Apt,
                source: "test index".into(),
                version: None,
                description: Some("recursive search tool".into()),
                score: 0,
                match_kind: MatchKind::None,
                verified: true,
            }])
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
            Ok(Vec::new())
        }
    }

    #[test]
    fn rejects_option_like_package_ids() {
        assert!(validate_package_id("ripgrep").is_ok());
        assert!(validate_package_id("--config").is_err());
        assert!(validate_package_id("bad\nname").is_err());
    }

    #[test]
    fn rejects_option_like_queries() {
        assert!(super::validate_query("ripgrep").is_ok());
        assert!(super::validate_query("--help").is_err());
        assert!(super::validate_query(&"x".repeat(129)).is_err());
    }

    #[test]
    fn broad_search_adapters_enable_bounded_fuzzy_fallback() {
        let registry = Registry::standard();
        for kind in [
            ManagerKind::Apt,
            ManagerKind::Pacman,
            ManagerKind::Brew,
            ManagerKind::Cargo,
            ManagerKind::Npm,
        ] {
            assert!(registry.adapter(kind).unwrap().supports_fuzzy_fallback());
        }
        for kind in [ManagerKind::Pip, ManagerKind::Uv, ManagerKind::Npx] {
            assert!(!registry.adapter(kind).unwrap().supports_fuzzy_fallback());
        }
    }

    #[tokio::test]
    async fn default_search_skips_execution_only_npx_instances() {
        let registry = Registry::standard();
        let instance = ManagerInstance {
            id: "npx:test".into(),
            kind: ManagerKind::Npx,
            executable: PathBuf::from("npx"),
            launcher_args: Vec::new(),
            version: "11.9.0".into(),
            scope: InstanceScope::Generic,
        };

        let (candidates, warnings) = registry.search(&[instance], &["typescript".into()]).await;
        assert!(candidates.is_empty());
        assert!(warnings.is_empty());
        assert!(registry.ensure_searchable(&[ManagerKind::Npx]).is_err());
        assert!(registry.ensure_installable(&[ManagerKind::Npx]).is_err());
    }

    #[tokio::test]
    async fn registry_recovers_a_typo_with_bounded_fallback_queries() {
        let registry = Registry {
            adapters: vec![Arc::new(FuzzyFallbackAdapter {
                fail_primary: false,
            })],
        };
        let instance = ManagerInstance {
            id: "apt:test".into(),
            kind: ManagerKind::Apt,
            executable: PathBuf::from("apt"),
            launcher_args: Vec::new(),
            version: "test".into(),
            scope: InstanceScope::Generic,
        };

        let (candidates, warnings) = registry.search(&[instance], &["ripgrpe".into()]).await;

        assert!(warnings.is_empty());
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].query, "ripgrpe");
        assert_eq!(candidates[0].package, "ripgrep");
        assert_eq!(candidates[0].match_kind, MatchKind::Edit);
        assert_eq!(candidates[1].match_kind, MatchKind::Description);
    }

    #[tokio::test]
    async fn registry_preserves_primary_errors_after_fuzzy_recovery() {
        let registry = Registry {
            adapters: vec![Arc::new(FuzzyFallbackAdapter { fail_primary: true })],
        };
        let instance = ManagerInstance {
            id: "apt:test".into(),
            kind: ManagerKind::Apt,
            executable: PathBuf::from("apt"),
            launcher_args: Vec::new(),
            version: "test".into(),
            scope: InstanceScope::Generic,
        };

        let (candidates, warnings) = registry.search(&[instance], &["ripgrpe".into()]).await;

        assert_eq!(candidates[0].package, "ripgrep");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("primary search failed"));
        assert!(warnings[0].contains("simulated primary failure"));
    }
}
