use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use clap::ValueEnum;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ManagerKind {
    Apt,
    Pacman,
    Brew,
    Cargo,
    Npm,
    Npx,
    Pip,
    Uv,
}

impl fmt::Display for ManagerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Apt => "apt",
            Self::Pacman => "pacman",
            Self::Brew => "brew",
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Npx => "npx",
            Self::Pip => "pip",
            Self::Uv => "uv",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatchKind {
    None,
    Provider,
    Description,
    Edit,
    Contains,
    Tokens,
    Prefix,
    CompactExact,
    NormalizedExact,
    CanonicalExact,
    Exact,
}

impl fmt::Display for MatchKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::None => "none",
            Self::Provider => "provider",
            Self::Description => "description",
            Self::Edit => "edit",
            Self::Contains => "contains",
            Self::Tokens => "tokens",
            Self::Prefix => "prefix",
            Self::CompactExact => "compact-exact",
            Self::NormalizedExact => "normalized-exact",
            Self::CanonicalExact => "canonical-exact",
            Self::Exact => "exact",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstanceScope {
    Generic,
    PythonVenv(PathBuf),
    NodeGlobal(PathBuf),
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagerInstance {
    pub id: String,
    pub kind: ManagerKind,
    pub executable: PathBuf,
    pub launcher_args: Vec<String>,
    pub version: String,
    pub scope: InstanceScope,
}

impl ManagerInstance {
    pub fn prepend_launcher(&self, args: Vec<String>) -> Vec<String> {
        let mut invocation = Vec::with_capacity(self.launcher_args.len() + args.len());
        invocation.extend(self.launcher_args.iter().cloned());
        invocation.extend(args);
        invocation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetKind {
    System,
    User,
    CargoRoot,
    NodeGlobal,
    NodeProject,
    NodeWorkspace,
    PythonVenv,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Target {
    pub id: String,
    pub kind: TargetKind,
    pub label: String,
    pub path: Option<PathBuf>,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub query: String,
    pub package: String,
    pub match_name: String,
    pub manager_instance_id: String,
    pub manager: ManagerKind,
    pub source: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub score: u16,
    pub match_kind: MatchKind,
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Selection {
    pub query: String,
    pub candidate: Candidate,
    pub target: Target,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandSpec {
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub env_remove_prefixes: Vec<String>,
    pub must_not_exist: Option<PathBuf>,
    pub requires_admin: bool,
}

impl CommandSpec {
    pub fn display_command(&self) -> String {
        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(display_arg(&self.program.to_string_lossy()));
        parts.extend(self.args.iter().map(|arg| display_arg(arg)));
        parts.join(" ")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallPlan {
    pub selections: Vec<Selection>,
    pub steps: Vec<CommandSpec>,
}

fn display_arg(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        if ch.is_control() {
            escaped.extend(ch.escape_default());
        } else {
            escaped.push(ch);
        }
    }
    if !escaped.is_empty()
        && escaped
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@+=,".contains(c))
    {
        escaped
    } else {
        format!("'{}'", escaped.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use super::display_arg;

    #[test]
    fn command_display_quotes_only_for_humans() {
        assert_eq!(display_arg("ripgrep"), "ripgrep");
        assert_eq!(display_arg("hello world"), "'hello world'");
        assert_eq!(display_arg("it's"), "'it'\"'\"'s'");
    }
}
