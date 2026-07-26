use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Resolve a path while keeping the most interoperable representation.
///
/// `std::fs::canonicalize` returns verbatim (`\\?\`) paths on Windows. Those
/// paths are useful for some filesystem operations but make unstable user
/// facing ids and are not accepted by every child process. `dunce` keeps the
/// regular drive-letter form whenever that conversion is unambiguous.
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    dunce::canonicalize(path)
}

pub fn canonicalize_or(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn path_entry_exists(path: impl AsRef<Path>) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Resolve the existing part of an absolute path and safely retain its missing suffix.
pub fn resolve_for_creation(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = path.as_ref();
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path to create must be absolute",
        ));
    }
    if path_entry_exists(path) {
        return canonicalize(path);
    }

    let mut cursor = path;
    let mut suffix = Vec::<OsString>::new();
    loop {
        if let Ok(mut resolved) = canonicalize(cursor) {
            for component in suffix.into_iter().rev() {
                resolved.push(component);
            }
            return Ok(resolved);
        }
        let name = cursor.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "path has no resolvable existing ancestor",
            )
        })?;
        suffix.push(name.to_os_string());
        cursor = cursor.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "path has no resolvable existing ancestor",
            )
        })?;
    }
}

pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let variables = ["USERPROFILE", "HOME"];

    #[cfg(not(windows))]
    let variables = ["HOME"];

    variables.into_iter().find_map(|name| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .and_then(|path| canonicalize(path).ok())
    })
}

pub const fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

pub fn env_key_has_prefix(key: &str, prefix: &str) -> bool {
    if cfg!(windows) {
        key.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    } else {
        key.starts_with(prefix)
    }
}

pub fn temporary_roots() -> Vec<PathBuf> {
    let mut candidates = vec![std::env::temp_dir()];

    #[cfg(windows)]
    {
        candidates.extend(
            ["TMP", "TEMP"]
                .into_iter()
                .filter_map(std::env::var_os)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        );
        candidates.extend(
            [
                ("LOCALAPPDATA", &["Temp"][..]),
                ("USERPROFILE", &["AppData", "Local", "Temp"][..]),
                ("SystemRoot", &["Temp"][..]),
                ("WINDIR", &["Temp"][..]),
            ]
            .into_iter()
            .filter_map(|(variable, suffix)| {
                std::env::var_os(variable)
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .map(|mut root| {
                        for component in suffix {
                            root.push(*component);
                        }
                        root
                    })
            }),
        );
    }

    #[cfg(unix)]
    for root in ["/tmp", "/var/tmp", "/dev/shm"] {
        let root = Path::new(root);
        if root.exists() {
            candidates.push(root.to_path_buf());
        }
    }

    let mut roots = Vec::new();
    for candidate in candidates {
        if let Ok(canonical) = canonicalize(candidate) {
            if !roots.contains(&canonical) {
                roots.push(canonical);
            }
        }
    }

    roots
}

#[cfg(test)]
mod tests {
    use super::{env_key_has_prefix, null_device, resolve_for_creation};

    #[test]
    fn null_device_matches_the_platform() {
        #[cfg(windows)]
        assert_eq!(null_device(), "NUL");

        #[cfg(not(windows))]
        assert_eq!(null_device(), "/dev/null");
    }

    #[test]
    fn environment_prefix_matching_matches_platform_rules() {
        assert!(env_key_has_prefix("PIP_INDEX_URL", "PIP_"));

        #[cfg(windows)]
        assert!(env_key_has_prefix("pip_index_url", "PIP_"));

        #[cfg(not(windows))]
        assert!(!env_key_has_prefix("pip_index_url", "PIP_"));
    }

    #[test]
    fn resolves_missing_suffix_from_an_existing_ancestor() {
        let temp = tempfile::tempdir().unwrap();
        let resolved = resolve_for_creation(temp.path().join("new").join("nested")).unwrap();
        assert_eq!(
            resolved,
            super::canonicalize(temp.path())
                .unwrap()
                .join("new")
                .join("nested")
        );
        assert!(resolve_for_creation("relative/path").is_err());
    }
}
