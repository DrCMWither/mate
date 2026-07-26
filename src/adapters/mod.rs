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
use crate::model::{Candidate, CommandSpec, ManagerInstance, ManagerKind, Target};

#[async_trait]
pub trait ManagerAdapter: Send + Sync {
    fn kind(&self) -> ManagerKind;

    fn supports_search(&self) -> bool {
        true
    }

    fn supports_install(&self) -> bool {
        true
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
                    let result = adapter.search(&instance, &query).await;
                    (instance.id, query, result)
                });
            }
        }

        let mut candidates = Vec::new();
        let mut warnings = BTreeSet::new();
        let results = stream::iter(jobs)
            .buffer_unordered(8)
            .collect::<Vec<_>>()
            .await;
        for (instance_id, query, result) in results {
            match result {
                Ok(mut found) => candidates.append(&mut found),
                Err(error) => {
                    warnings.insert(format!("{instance_id}, query {query:?}: {error:#}"));
                }
            }
        }

        candidates.sort_by(|a, b| {
            a.query
                .cmp(&b.query)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.manager.cmp(&b.manager))
                .then_with(|| a.package.cmp(&b.package))
        });
        (candidates, warnings.into_iter().collect())
    }
}

pub fn score_match(query: &str, package: &str) -> u16 {
    if package.eq_ignore_ascii_case(query) {
        100
    } else if package
        .to_ascii_lowercase()
        .starts_with(&query.to_ascii_lowercase())
    {
        80
    } else {
        60
    }
}

pub fn validate_query(query: &str) -> Result<()> {
    if query.trim().is_empty() {
        return Err(anyhow!("package query cannot be empty"));
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

    use super::{validate_package_id, Registry};
    use crate::model::{InstanceScope, ManagerInstance, ManagerKind};

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
}
