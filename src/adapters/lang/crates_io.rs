use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::header::ACCEPT;
use serde::Deserialize;

const SEARCH_ENDPOINT: &str = "https://crates.io/api/v1/crates";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CratePackage {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    crates: Vec<SearchCrate>,
}

#[derive(Debug, Deserialize)]
struct SearchCrate {
    name: Option<String>,
    max_version: Option<String>,
    description: Option<String>,
}

pub async fn search(query: &str) -> Result<Vec<CratePackage>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(concat!("mate/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("cannot initialize crates.io client")?;
    let response = client
        .get(SEARCH_ENDPOINT)
        .header(ACCEPT, "application/json")
        .query(&[("q", query), ("per_page", "40")])
        .send()
        .await
        .context("crates.io request failed")?
        .error_for_status()
        .context("crates.io returned an error")?;

    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(anyhow!("crates.io response exceeds the safety limit"));
    }

    let mut response = response;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("cannot read crates.io response")?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(anyhow!("crates.io response exceeds the safety limit"));
        }
        body.extend_from_slice(&chunk);
    }
    parse_response(&body)
}

fn parse_response(body: &[u8]) -> Result<Vec<CratePackage>> {
    let response: SearchResponse =
        serde_json::from_slice(body).context("invalid crates.io response")?;
    Ok(response
        .crates
        .into_iter()
        .filter_map(|package| {
            let name = package.name?;
            let version = package.max_version?;
            if !valid_crate_name(&name) || semver::Version::parse(&version).is_err() {
                return None;
            }
            Some(CratePackage {
                name,
                version,
                description: package
                    .description
                    .filter(|description| !description.trim().is_empty()),
            })
        })
        .take(40)
        .collect())
}

pub fn valid_crate_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::{parse_response, valid_crate_name};

    #[test]
    fn parses_forward_compatible_registry_results() {
        let body = br#"{
            "crates": [
                {
                    "name": "ripgrep",
                    "max_version": "14.1.1",
                    "description": "fast search",
                    "future_field": true
                },
                {"name": "empty-summary", "max_version": "1.0.0", "description": ""},
                {"name": "missing-version"},
                {"name": "bad@name", "max_version": "1.0.0"}
            ],
            "meta": {"total": 4}
        }"#;
        let packages = parse_response(body).unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "ripgrep");
        assert_eq!(packages[0].version, "14.1.1");
        assert_eq!(packages[1].description, None);
    }

    #[test]
    fn validates_crate_names_and_semver_input_boundary() {
        assert!(valid_crate_name("cargo-edit"));
        assert!(valid_crate_name("cargo_edit2"));
        assert!(!valid_crate_name(""));
        assert!(!valid_crate_name("--config=evil"));
        assert!(!valid_crate_name("bad@crate"));
        assert!(!valid_crate_name("包"));
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_response(b"{").is_err());
    }
}
