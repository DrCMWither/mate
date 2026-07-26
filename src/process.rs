use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::model::CommandSpec;
use crate::platform;

const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_OUTPUT_LIMIT: usize = 64 * 1024;
const SEARCH_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct CapturedOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub async fn probe_version<S: AsRef<OsStr>>(program: &Path, args: &[S]) -> String {
    probe_version_sanitized(program, args, &[], &[]).await
}

pub async fn probe_version_sanitized<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
    env_remove_prefixes: &[&str],
    env: &[(&str, &str)],
) -> String {
    probe_version_sanitized_in(program, args, env_remove_prefixes, env, None).await
}

pub async fn probe_version_sanitized_in<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
    env_remove_prefixes: &[&str],
    env: &[(&str, &str)],
    cwd: Option<&Path>,
) -> String {
    match capture(
        program,
        args,
        PROBE_TIMEOUT,
        PROBE_OUTPUT_LIMIT,
        env_remove_prefixes,
        env,
        cwd,
    )
    .await
    {
        Ok(output) => output
            .stdout
            .lines()
            .chain(output.stderr.lines())
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("version unknown")
            .to_owned(),
        Err(_) => "version unknown".to_owned(),
    }
}

pub async fn search_command<S: AsRef<OsStr>>(program: &Path, args: &[S]) -> Result<CapturedOutput> {
    capture(
        program,
        args,
        SEARCH_TIMEOUT,
        SEARCH_OUTPUT_LIMIT,
        &[],
        &[],
        None,
    )
    .await
}

pub async fn search_command_sanitized<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
    env_remove_prefixes: &[&str],
    env: &[(&str, &str)],
) -> Result<CapturedOutput> {
    capture(
        program,
        args,
        SEARCH_TIMEOUT,
        SEARCH_OUTPUT_LIMIT,
        env_remove_prefixes,
        env,
        None,
    )
    .await
}

async fn capture<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
    duration: Duration,
    output_limit: usize,
    env_remove_prefixes: &[&str],
    env: &[(&str, &str)],
    cwd: Option<&Path>,
) -> Result<CapturedOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    for (key, _) in std::env::vars_os() {
        let key_text = key.to_string_lossy();
        if env_remove_prefixes
            .iter()
            .any(|prefix| platform::env_key_has_prefix(&key_text, prefix))
        {
            command.env_remove(&key);
        }
    }
    for &(key, value) in env {
        command.env(key, value);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run {}", program.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture stdout from {}", program.display()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture stderr from {}", program.display()))?;
    let captured_bytes = Arc::new(AtomicUsize::new(0));

    let captured = timeout(duration, async {
        tokio::try_join!(
            async {
                child
                    .wait()
                    .await
                    .with_context(|| format!("failed to wait for {}", program.display()))
            },
            async {
                read_bounded(stdout, output_limit, Arc::clone(&captured_bytes))
                    .await
                    .map_err(|error| capture_read_error(program, "stdout", output_limit, error))
            },
            async {
                read_bounded(stderr, output_limit, Arc::clone(&captured_bytes))
                    .await
                    .map_err(|error| capture_read_error(program, "stderr", output_limit, error))
            },
        )
    })
    .await;

    let (status, stdout, stderr) = match captured {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            terminate_and_reap(&mut child).await;
            return Err(error);
        }
        Err(_) => {
            terminate_and_reap(&mut child).await;
            return Err(anyhow!(
                "{} timed out after {} seconds",
                program.display(),
                duration.as_secs()
            ));
        }
    };

    Ok(CapturedOutput {
        success: status.success(),
        code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

#[derive(Debug)]
enum BoundedReadError {
    Io(io::Error),
    LimitExceeded,
}

async fn read_bounded<R>(
    mut reader: R,
    limit: usize,
    captured_bytes: Arc<AtomicUsize>,
) -> std::result::Result<Vec<u8>, BoundedReadError>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::with_capacity(limit.min(8 * 1024));
    let mut chunk = [0_u8; 8 * 1024];

    loop {
        let remaining = limit.saturating_sub(captured_bytes.load(Ordering::Relaxed));
        let read_size = chunk.len().min(remaining.saturating_add(1));
        let count = reader
            .read(&mut chunk[..read_size])
            .await
            .map_err(BoundedReadError::Io)?;
        if count == 0 {
            return Ok(output);
        }
        let reservation =
            captured_bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |captured| {
                captured
                    .checked_add(count)
                    .filter(|new_total| *new_total <= limit)
            });
        if reservation.is_err() {
            return Err(BoundedReadError::LimitExceeded);
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

fn capture_read_error(
    program: &Path,
    stream: &str,
    limit: usize,
    error: BoundedReadError,
) -> anyhow::Error {
    match error {
        BoundedReadError::Io(error) => anyhow!(error).context(format!(
            "failed to read {stream} from {}",
            program.display()
        )),
        BoundedReadError::LimitExceeded => anyhow!(
            "{} combined stdout/stderr exceeded the {limit}-byte capture limit; process terminated",
            program.display()
        ),
    }
}

async fn terminate_and_reap(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

pub fn find_all_on_path(name: &str) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    find_all_in_directories(name, std::env::split_paths(&path))
}

fn find_all_in_directories(
    name: &str,
    directories: impl IntoIterator<Item = PathBuf>,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut identities = Vec::new();
    let names = executable_names(name);
    for mut directory in directories {
        if directory.as_os_str().is_empty() {
            let Ok(cwd) = std::env::current_dir() else {
                continue;
            };
            directory = cwd;
        }
        let Ok(launch_directory) = platform::canonicalize(&directory) else {
            continue;
        };
        for candidate_name in &names {
            let candidate = launch_directory.join(candidate_name);
            if !candidate.is_file() {
                continue;
            }
            let Ok(identity) = platform::canonicalize(&candidate) else {
                continue;
            };
            if identities.contains(&identity) {
                continue;
            }
            identities.push(identity);
            found.push(candidate);
        }
    }
    found
}

pub fn safe_diagnostic(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars().take(2_000) {
        if ch.is_control() {
            output.extend(ch.escape_default());
        } else {
            output.push(ch);
        }
    }
    output
}

pub fn safe_user_executable(path: &Path, project_root: Option<&Path>) -> bool {
    let Ok(canonical) = platform::canonicalize(path) else {
        return false;
    };
    let in_temporary_directory = platform::temporary_roots()
        .into_iter()
        .any(|root| canonical.starts_with(root));
    if project_root.is_some_and(|root| canonical.starts_with(root)) || in_temporary_directory {
        return false;
    }

    user_executable_permissions_are_safe(&canonical)
}

#[cfg(unix)]
fn user_executable_permissions_are_safe(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| {
            let mode = metadata.permissions().mode();
            metadata.is_file() && mode & 0o111 != 0 && mode & 0o022 == 0
        })
        .unwrap_or(false)
}

#[cfg(windows)]
fn user_executable_permissions_are_safe(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
}

#[cfg(not(any(unix, windows)))]
fn user_executable_permissions_are_safe(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
pub fn find_trusted_system_executable(name: &str) -> Option<PathBuf> {
    ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        .into_iter()
        .map(|directory| Path::new(directory).join(name))
        .filter(|candidate| candidate.is_file())
        .filter_map(|candidate| platform::canonicalize(candidate).ok())
        .find(|candidate| trusted_root_owned_file(candidate))
}

#[cfg(not(unix))]
pub fn find_trusted_system_executable(_name: &str) -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn trusted_root_owned_file(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    std::fs::metadata(path)
        .map(|metadata| {
            let mode = metadata.permissions().mode();
            metadata.is_file() && metadata.uid() == 0 && mode & 0o111 != 0 && mode & 0o022 == 0
        })
        .unwrap_or(false)
}

#[cfg(windows)]
fn executable_names(name: &str) -> Vec<String> {
    vec![format!("{name}.exe")]
}

#[cfg(not(windows))]
fn executable_names(name: &str) -> Vec<String> {
    vec![name.to_owned()]
}

pub async fn execute_steps(steps: &[CommandSpec]) -> Result<()> {
    let root = is_root();
    if root && steps.iter().any(|step| !step.requires_admin) {
        return Err(anyhow!(
            "refusing to run user or virtual-environment installation steps as root; run mate as the target user"
        ));
    }
    for step in steps {
        verify_preconditions(step)?;
    }

    for (index, step) in steps.iter().enumerate() {
        verify_preconditions(step)?;
        eprintln!(
            "\n[{}/{}] {}: {}",
            index + 1,
            steps.len(),
            step.label,
            step.display_command()
        );

        let mut command = if step.requires_admin && !root {
            let sudo = find_trusted_system_executable("sudo")
                .ok_or_else(|| anyhow!("trusted system sudo executable was not found"))?;
            let mut elevated = Command::new(sudo);
            elevated.arg("--").arg(&step.program).args(&step.args);
            elevated
        } else {
            let mut direct = Command::new(&step.program);
            direct.args(&step.args);
            direct
        };

        if let Some(cwd) = &step.cwd {
            command.current_dir(cwd);
        }
        for (key, _) in std::env::vars_os() {
            let key_text = key.to_string_lossy();
            if step
                .env_remove_prefixes
                .iter()
                .any(|prefix| platform::env_key_has_prefix(&key_text, prefix))
            {
                command.env_remove(&key);
            }
        }
        command.envs(&step.env);
        command.stdin(Stdio::inherit());
        command.stdout(Stdio::inherit());
        command.stderr(Stdio::inherit());

        let status = command
            .status()
            .await
            .with_context(|| format!("failed to execute {}", step.program.display()))?;
        if !status.success() {
            return Err(anyhow!("step {} failed with status {}", index + 1, status));
        }
    }

    Ok(())
}

fn verify_preconditions(step: &CommandSpec) -> Result<()> {
    if let Some(path) = &step.must_not_exist {
        if platform::path_entry_exists(path) {
            return Err(anyhow!(
                "plan precondition changed: {} now exists; refusing to create or overwrite it",
                safe_diagnostic(&path.to_string_lossy())
            ));
        }
    }
    Ok(())
}

pub fn ensure_unprivileged() -> Result<()> {
    if is_root() {
        return Err(anyhow!(
            "mate must not run as root; run it as the target user and let mate elevate only confirmed system-package steps"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::{capture, executable_names, read_bounded};
    #[cfg(unix)]
    use super::{find_all_in_directories, verify_preconditions};

    #[test]
    fn manager_discovery_uses_only_directly_executable_files() {
        #[cfg(windows)]
        assert_eq!(executable_names("uv"), vec!["uv.exe"]);

        #[cfg(not(windows))]
        assert_eq!(executable_names("uv"), vec!["uv"]);
    }

    #[tokio::test]
    async fn bounded_reader_accepts_exact_limit() {
        let input = b"exactly eight";
        let captured_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let output = read_bounded(&input[..], input.len(), captured_bytes)
            .await
            .unwrap();
        assert_eq!(output, input);
    }

    #[tokio::test]
    async fn bounded_readers_share_one_combined_budget() {
        let stdout = vec![b'o'; 600];
        let stderr = vec![b'e'; 600];
        let captured_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let result = tokio::try_join!(
            read_bounded(&stdout[..], 1_000, Arc::clone(&captured_bytes)),
            read_bounded(&stderr[..], 1_000, captured_bytes),
        );

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn capture_drains_stdout_and_stderr_concurrently() {
        let executable = std::env::current_exe().unwrap();
        let args = [
            "--exact",
            "process::tests::capture_subprocess_helper",
            "--ignored",
            "--nocapture",
        ];
        let output = capture(
            &executable,
            &args,
            Duration::from_secs(5),
            1024 * 1024,
            &[],
            &[("MATE_CAPTURE_TEST_MODE", "dual")],
            None,
        )
        .await
        .unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("STDOUT_DONE"));
        assert!(output.stderr.contains("STDERR_DONE"));
    }

    #[tokio::test]
    async fn capture_terminates_a_process_that_exceeds_the_limit() {
        let executable = std::env::current_exe().unwrap();
        let args = [
            "--exact",
            "process::tests::capture_subprocess_helper",
            "--ignored",
            "--nocapture",
        ];
        let error = capture(
            &executable,
            &args,
            Duration::from_secs(5),
            1024,
            &[],
            &[("MATE_CAPTURE_TEST_MODE", "over-limit")],
            None,
        )
        .await
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("combined stdout/stderr exceeded the 1024-byte capture limit"));
        assert!(message.contains("process terminated"));
    }

    #[test]
    #[ignore = "subprocess fixture invoked by the capture tests"]
    fn capture_subprocess_helper() {
        match std::env::var("MATE_CAPTURE_TEST_MODE").as_deref() {
            Ok("dual") => write_both_streams(),
            Ok("over-limit") => {
                let output = vec![b'x'; 128 * 1024];
                std::io::stdout().write_all(&output).unwrap();
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_secs(30));
            }
            _ => {}
        }
    }

    fn write_both_streams() {
        let barrier = Arc::new(Barrier::new(2));
        let stdout_barrier = Arc::clone(&barrier);
        let stdout = std::thread::spawn(move || {
            let mut stream = std::io::stdout().lock();
            let output = vec![b'o'; 256 * 1024];
            stream.write_all(&output).unwrap();
            stream.flush().unwrap();
            stdout_barrier.wait();
            stream.write_all(b"STDOUT_DONE\n").unwrap();
        });
        let stderr = std::thread::spawn(move || {
            let mut stream = std::io::stderr().lock();
            let output = vec![b'e'; 256 * 1024];
            stream.write_all(&output).unwrap();
            stream.flush().unwrap();
            barrier.wait();
            stream.write_all(b"STDERR_DONE\n").unwrap();
        });

        stdout.join().unwrap();
        stderr.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn discovery_preserves_rustup_proxy_launcher_name() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let rustup = temp.path().join("rustup");
        std::fs::write(&rustup, b"#!/bin/sh\n").unwrap();
        let mut permissions = std::fs::metadata(&rustup).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&rustup, permissions).unwrap();
        symlink(&rustup, temp.path().join("cargo")).unwrap();

        let found = find_all_in_directories("cargo", [temp.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file_name().unwrap(), "cargo");
    }

    #[cfg(unix)]
    #[test]
    fn creation_precondition_rejects_a_broken_symlink() {
        use std::collections::BTreeMap;
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("environment");
        symlink(temp.path().join("missing"), &target).unwrap();
        let step = crate::model::CommandSpec {
            label: "test".into(),
            program: "uv".into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            env_remove_prefixes: Vec::new(),
            must_not_exist: Some(target),
            requires_admin: false,
        };
        assert!(verify_preconditions(&step).is_err());
    }
}
