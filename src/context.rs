use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::model::{Target, TargetKind};
use crate::platform;

#[derive(Debug, Clone, Serialize)]
pub struct ProjectContext {
    pub cwd: PathBuf,
    pub project_root: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
    pub targets: Vec<Target>,
    pub markers: Vec<String>,
}

impl ProjectContext {
    pub fn discover() -> Result<Self> {
        let cwd = std::env::current_dir().context("cannot determine current directory")?;
        let cwd = platform::canonicalize(&cwd)
            .context("cannot resolve the current directory to a stable path")?;
        let (project_root, workspace_root, markers) = locate_project(&cwd);
        let target_base = project_root.as_deref().unwrap_or(&cwd);
        let mut targets = vec![
            Target {
                id: "system".into(),
                kind: TargetKind::System,
                label: "system packages".into(),
                path: None,
                exists: true,
            },
            Target {
                id: "user".into(),
                kind: TargetKind::User,
                label: "current user".into(),
                path: None,
                exists: true,
            },
        ];

        add_cargo_targets(&mut targets, &cwd);
        add_node_targets(&mut targets, &cwd);

        let mut seen_python = BTreeSet::new();
        let mut has_configured_new_venv = false;
        for variable in ["VIRTUAL_ENV", "CONDA_PREFIX", "UV_PROJECT_ENVIRONMENT"] {
            if let Some(active) = environment_path(variable, &cwd) {
                has_configured_new_venv |=
                    add_configured_python_target(&mut targets, &mut seen_python, variable, active);
            }
        }

        for name in [".venv", "venv", "env"] {
            let path = target_base.join(name);
            if let Some(environment) = detect_python_environment(&path) {
                add_python_target(&mut targets, &mut seen_python, path, environment, true);
            }
        }

        let default_venv = target_base.join(".venv");
        if !has_configured_new_venv && !platform::path_entry_exists(&default_venv) {
            add_python_target(
                &mut targets,
                &mut seen_python,
                default_venv,
                PythonEnvironment::Venv,
                false,
            );
        }

        Ok(Self {
            cwd,
            project_root,
            workspace_root,
            targets,
            markers,
        })
    }
}

fn locate_project(cwd: &Path) -> (Option<PathBuf>, Option<PathBuf>, Vec<String>) {
    let home = platform::home_dir();
    let manifest_names = [
        "pyproject.toml",
        "uv.lock",
        "requirements.txt",
        "Pipfile",
        "package.json",
        "Cargo.toml",
    ];
    let vcs_names = [".git", ".hg", ".svn"];
    let mut nearest_manifest = None;
    let mut nearest_vcs = None;

    for ancestor in cwd.ancestors() {
        let canonical = ancestor.to_path_buf();
        if home.as_ref() == Some(&canonical) {
            break;
        }
        if nearest_manifest.is_none() {
            let markers = manifest_names
                .iter()
                .filter(|name| ancestor.join(name).exists())
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>();
            if !markers.is_empty() {
                nearest_manifest = Some((canonical.clone(), markers));
            }
        }
        if nearest_vcs.is_none() {
            let markers = vcs_names
                .iter()
                .filter(|name| ancestor.join(name).exists())
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>();
            if !markers.is_empty() {
                nearest_vcs = Some((canonical, markers));
            }
        }
    }

    let project = nearest_manifest.as_ref().or(nearest_vcs.as_ref());
    let project_root = project.map(|(path, _)| path.clone());
    let markers = project
        .map(|(_, markers)| markers.clone())
        .unwrap_or_default();
    let workspace_root = nearest_vcs
        .map(|(path, _)| path)
        .or_else(|| project_root.clone());

    (project_root, workspace_root, markers)
}

pub fn venv_python(path: &Path) -> PathBuf {
    if cfg!(windows) {
        path.join("Scripts").join("python.exe")
    } else {
        path.join("bin").join("python")
    }
}

pub fn python_environment_python(path: &Path) -> Option<PathBuf> {
    let candidates = if cfg!(windows) {
        vec![
            path.join("Scripts").join("python.exe"),
            path.join("python.exe"),
        ]
    } else {
        vec![
            path.join("bin").join("python"),
            path.join("bin").join("python3"),
        ]
    };
    candidates.into_iter().find(|candidate| candidate.is_file())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PythonEnvironment {
    Venv,
    Conda,
}

fn detect_python_environment(path: &Path) -> Option<PythonEnvironment> {
    let has_python = python_environment_python(path).is_some();
    if has_python && path.join("pyvenv.cfg").is_file() {
        Some(PythonEnvironment::Venv)
    } else if has_python && path.join("conda-meta").is_dir() {
        Some(PythonEnvironment::Conda)
    } else {
        None
    }
}

fn add_configured_python_target(
    targets: &mut Vec<Target>,
    seen: &mut BTreeSet<PathBuf>,
    variable: &str,
    path: PathBuf,
) -> bool {
    if let Some(environment) = detect_python_environment(&path) {
        add_python_target(targets, seen, path, environment, true);
        false
    } else if variable == "UV_PROJECT_ENVIRONMENT" && !platform::path_entry_exists(&path) {
        add_python_target(targets, seen, path, PythonEnvironment::Venv, false);
        true
    } else {
        false
    }
}

fn add_python_target(
    targets: &mut Vec<Target>,
    seen: &mut BTreeSet<PathBuf>,
    path: PathBuf,
    environment: PythonEnvironment,
    exists: bool,
) {
    let absolute = if exists {
        let Ok(absolute) = platform::canonicalize(&path) else {
            return;
        };
        absolute
    } else {
        let Ok(absolute) = platform::resolve_for_creation(&path) else {
            return;
        };
        absolute
    };
    if !seen.insert(absolute.clone()) {
        return;
    }

    targets.push(Target {
        id: format!("venv:{}", absolute.display()),
        kind: TargetKind::PythonVenv,
        label: if exists {
            match environment {
                PythonEnvironment::Venv => {
                    format!("Python venv {}", absolute.display())
                }
                PythonEnvironment::Conda => {
                    format!("Conda environment {}", absolute.display())
                }
            }
        } else {
            format!("create Python venv {}", absolute.display())
        },
        path: Some(absolute),
        exists,
    });
}

fn environment_path(variable: &str, cwd: &Path) -> Option<PathBuf> {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
}

fn add_cargo_targets(targets: &mut Vec<Target>, cwd: &Path) {
    let install_root = environment_path("CARGO_INSTALL_ROOT", cwd);
    let cargo_home = environment_path("CARGO_HOME", cwd);
    let home = platform::home_dir();
    if let Some((root, source)) =
        select_cargo_install_root(cwd, install_root, cargo_home, home.as_deref())
    {
        add_cargo_target(targets, root, format!("Cargo install root ({source})"));
    }

    if let Some(project) = nearest_marker_root(cwd, "Cargo.toml") {
        let root = project.join(".mate").join("cargo");
        add_cargo_target(targets, root, "project Cargo install root".into());
    }
}

fn select_cargo_install_root(
    cwd: &Path,
    install_root: Option<PathBuf>,
    cargo_home: Option<PathBuf>,
    home: Option<&Path>,
) -> Option<(PathBuf, &'static str)> {
    let (path, source) = if let Some(path) = install_root {
        (path, "CARGO_INSTALL_ROOT")
    } else if let Some(path) = cargo_home {
        (path, "CARGO_HOME")
    } else {
        (home?.join(".cargo"), "HOME")
    };
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    platform::resolve_for_creation(path)
        .ok()
        .map(|path| (path, source))
}

fn add_cargo_target(targets: &mut Vec<Target>, path: PathBuf, label: String) {
    let Ok(path) = platform::resolve_for_creation(path) else {
        return;
    };
    let id = format!("cargo-root:{}", path.display());
    if targets.iter().any(|target| target.id == id) {
        return;
    }
    targets.push(Target {
        id,
        kind: TargetKind::CargoRoot,
        label: format!("{label} {}", path.display()),
        path: Some(path.clone()),
        exists: path.is_dir(),
    });
}

fn nearest_marker_root(cwd: &Path, marker: &str) -> Option<PathBuf> {
    let home = platform::home_dir();
    cwd.ancestors()
        .take_while(|ancestor| home.as_deref() != Some(*ancestor))
        .find(|ancestor| ancestor.join(marker).is_file())
        .map(Path::to_path_buf)
}

fn add_node_targets(targets: &mut Vec<Target>, cwd: &Path) {
    let home = platform::home_dir();
    let package_roots = cwd
        .ancestors()
        .take_while(|ancestor| home.as_deref() != Some(*ancestor))
        .filter(|ancestor| ancestor.join("package.json").is_file())
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    let Some(nearest) = package_roots.first() else {
        return;
    };
    let workspace = package_roots
        .iter()
        .find(|root| is_node_workspace(root))
        .cloned();

    match workspace {
        Some(workspace) if &workspace == nearest => {
            add_node_target(targets, workspace, TargetKind::NodeWorkspace);
        }
        Some(workspace) => {
            add_node_target(targets, nearest.clone(), TargetKind::NodeProject);
            add_node_target(targets, workspace, TargetKind::NodeWorkspace);
        }
        None => add_node_target(targets, nearest.clone(), TargetKind::NodeProject),
    }
}

fn add_node_target(targets: &mut Vec<Target>, path: PathBuf, kind: TargetKind) {
    let path = platform::canonicalize_or(path);
    let manager = node_manager_hint(&path);
    let dependencies = if path.join("node_modules").is_dir() {
        "dependencies present"
    } else {
        "dependencies not installed"
    };
    let environment = match kind {
        TargetKind::NodeWorkspace => "Node workspace",
        TargetKind::NodeProject => "Node project",
        _ => return,
    };
    let id_kind = match kind {
        TargetKind::NodeWorkspace => "node-workspace",
        TargetKind::NodeProject => "node-project",
        _ => return,
    };
    targets.push(Target {
        id: format!("{id_kind}:{}", path.display()),
        kind,
        label: format!(
            "{environment} {} ({manager}; {dependencies})",
            path.display()
        ),
        path: Some(path.clone()),
        exists: path.is_dir(),
    });
}

fn is_node_workspace(path: &Path) -> bool {
    path.join("pnpm-workspace.yaml").is_file()
        || path.join("lerna.json").is_file()
        || package_json(path)
            .and_then(|manifest| manifest.get("workspaces").cloned())
            .is_some_and(|workspaces| matches!(workspaces, Value::Array(_) | Value::Object(_)))
}

fn package_json(path: &Path) -> Option<Value> {
    let manifest = path.join("package.json");
    if std::fs::metadata(&manifest).ok()?.len() > 1_048_576 {
        return None;
    }
    serde_json::from_slice(&std::fs::read(manifest).ok()?).ok()
}

fn node_manager_hint(path: &Path) -> String {
    if let Some(manager) = package_json(path)
        .and_then(|manifest| manifest.get("packageManager").cloned())
        .and_then(|manager| manager.as_str().map(str::to_owned))
        .and_then(|manager| manager.split('@').next().map(str::to_owned))
        .filter(|manager| !manager.is_empty())
    {
        manager
    } else if path.join("pnpm-lock.yaml").is_file() {
        "pnpm".into()
    } else if path.join("yarn.lock").is_file() {
        "yarn".into()
    } else if path.join("bun.lock").is_file() || path.join("bun.lockb").is_file() {
        "bun".into()
    } else {
        "npm".into()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::{
        add_cargo_target, add_configured_python_target, add_node_targets,
        detect_python_environment, locate_project, select_cargo_install_root, PythonEnvironment,
    };
    use crate::model::TargetKind;
    use crate::platform;

    #[test]
    fn separates_nested_manifest_from_vcs_workspace() {
        let temp = tempdir().unwrap();
        let repository = temp.path().join("repo");
        let app = repository.join("packages").join("app");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("package.json"), "{}").unwrap();

        let canonical_app = platform::canonicalize(&app).unwrap();
        let (project_root, workspace_root, markers) = locate_project(&canonical_app);
        let expected_app = platform::canonicalize(&app).unwrap();
        let expected_repository = platform::canonicalize(&repository).unwrap();
        assert_eq!(project_root.as_deref(), Some(expected_app.as_path()));
        assert_eq!(
            workspace_root.as_deref(),
            Some(expected_repository.as_path())
        );
        assert_eq!(markers, vec!["package.json".to_owned()]);
    }

    #[test]
    fn detects_conda_environment_by_interpreter_and_metadata() {
        let temp = tempdir().unwrap();
        let environment = temp.path().join("conda");
        fs::create_dir_all(environment.join("conda-meta")).unwrap();
        let interpreter = if cfg!(windows) {
            environment.join("python.exe")
        } else {
            let bin = environment.join("bin");
            fs::create_dir_all(&bin).unwrap();
            bin.join("python")
        };
        fs::write(interpreter, b"").unwrap();

        assert_eq!(
            detect_python_environment(&environment),
            Some(PythonEnvironment::Conda)
        );
    }

    #[test]
    fn missing_uv_project_environment_becomes_a_creation_target() {
        let temp = tempdir().unwrap();
        let environment = temp.path().join("custom-python");
        let mut targets = Vec::new();
        let mut seen = BTreeSet::new();

        assert!(add_configured_python_target(
            &mut targets,
            &mut seen,
            "UV_PROJECT_ENVIRONMENT",
            environment.clone(),
        ));
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, TargetKind::PythonVenv);
        let expected = platform::resolve_for_creation(&environment).unwrap();
        assert_eq!(targets[0].path.as_ref(), Some(&expected));
        assert!(!targets[0].exists);

        let mut ignored = Vec::new();
        assert!(!add_configured_python_target(
            &mut ignored,
            &mut BTreeSet::new(),
            "VIRTUAL_ENV",
            temp.path().join("stale-venv"),
        ));
        assert!(ignored.is_empty());
    }

    #[test]
    fn detects_nearest_node_project_and_ancestor_workspace() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let app = workspace.join("packages").join("app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            workspace.join("package.json"),
            br#"{"private":true,"workspaces":["packages/*"],"packageManager":"npm@11.4.0"}"#,
        )
        .unwrap();
        fs::write(app.join("package.json"), br#"{"name":"app"}"#).unwrap();

        let mut targets = Vec::new();
        add_node_targets(&mut targets, &app);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].kind, TargetKind::NodeProject);
        assert_eq!(targets[1].kind, TargetKind::NodeWorkspace);
        assert!(targets[1].label.contains("(npm;"));
        assert!(targets.iter().all(|target| target.exists));
    }

    #[test]
    fn cargo_install_root_uses_documented_precedence() {
        let temp = tempdir().unwrap();
        let cwd = platform::canonicalize(temp.path()).unwrap();
        let install = PathBuf::from("install-root");
        let cargo_home = PathBuf::from("cargo-home");
        let home = cwd.join("home");
        fs::create_dir_all(&home).unwrap();

        let (selected, source) =
            select_cargo_install_root(&cwd, Some(install), Some(cargo_home), Some(&home)).unwrap();
        assert_eq!(selected, cwd.join("install-root"));
        assert_eq!(source, "CARGO_INSTALL_ROOT");

        let (selected, source) =
            select_cargo_install_root(&cwd, None, Some(PathBuf::from("cargo-home")), Some(&home))
                .unwrap();
        assert_eq!(selected, cwd.join("cargo-home"));
        assert_eq!(source, "CARGO_HOME");
    }

    #[test]
    fn cargo_targets_are_deduplicated_by_resolved_root() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let mut targets = Vec::new();
        add_cargo_target(&mut targets, root.clone(), "one".into());
        add_cargo_target(&mut targets, root.join("missing").join(".."), "two".into());
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, TargetKind::CargoRoot);
    }

    #[cfg(windows)]
    #[test]
    fn project_paths_do_not_leak_verbatim_prefixes() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("Cargo.toml"), "").unwrap();

        let canonical_project = platform::canonicalize(&project).unwrap();
        let (project_root, workspace_root, _) = locate_project(&canonical_project);
        for path in [project_root, workspace_root].into_iter().flatten() {
            assert!(
                !path.to_string_lossy().starts_with(r"\\?\"),
                "unexpected verbatim path: {}",
                path.display()
            );
        }
    }
}
