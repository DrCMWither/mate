use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct PypiPackage {
    pub name: String,
    pub version: String,
    pub summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PypiResponse {
    info: PypiInfo,
}

#[derive(Debug, Deserialize)]
struct PypiInfo {
    name: String,
    version: String,
    summary: Option<String>,
}

pub async fn exact_lookup(query: &str) -> Result<Option<PypiPackage>> {
    let normalized = normalize_name(query)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(concat!("mate/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("cannot initialize PyPI client")?;
    let url = format!("https://pypi.org/pypi/{normalized}/json");
    let response = client
        .get(url)
        .send()
        .await
        .context("PyPI request failed")?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .context("PyPI returned an error")?
        .json::<PypiResponse>()
        .await
        .context("invalid PyPI response")?;

    Ok(Some(PypiPackage {
        name: response.info.name,
        version: response.info.version,
        summary: response
            .info
            .summary
            .filter(|value| !value.trim().is_empty()),
    }))
}

fn normalize_name(query: &str) -> Result<String> {
    if query.is_empty()
        || !query
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(anyhow!(
            "Python prototype search accepts only an exact distribution name"
        ));
    }

    let mut normalized = String::new();
    let mut separator = false;
    for ch in query.chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !separator {
                normalized.push('-');
                separator = true;
            }
        } else {
            normalized.push(ch.to_ascii_lowercase());
            separator = false;
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::normalize_name;

    #[test]
    fn applies_python_name_normalization() {
        assert_eq!(normalize_name("My_Pkg.Name").unwrap(), "my-pkg-name");
        assert!(normalize_name("requests>=2").is_err());
    }
}
