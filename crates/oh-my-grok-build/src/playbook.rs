//! Deterministic CI-style playbooks for `omgb`.
//!
//! Playbooks are TOML or JSON files describing a sequence of steps that can be
//! run non-interactively. They are intended for headless / CI use where the
//! model, tools, and expected outcomes are pinned.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const SHELL_STEP_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_CAPTURE_BYTES: u64 = 2 * 1024 * 1024;

use xai_grok_pager::headless::OutputFormat;

use crate::args::PlaybookArgs;
use crate::{SessionParams, run_single_turn_with};

#[derive(Debug, Deserialize)]
struct Playbook {
    #[serde(default)]
    name: Option<String>,
    step: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Step {
    Exec(ExecStep),
    Shell(ShellStep),
    AssertFile(AssertFileStep),
    GitCommit(GitCommitStep),
}

#[derive(Debug, Deserialize)]
struct ExecStep {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    yolo: Option<bool>,
    #[serde(default)]
    tools: Option<String>,
    #[serde(default)]
    max_turns: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ShellStep {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    expect_exit: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct AssertFileStep {
    path: PathBuf,
    #[serde(default)]
    contains: Option<String>,
    #[serde(default)]
    exists: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GitCommitStep {
    message: String,
}

fn load_playbook(path: &Path) -> Result<Playbook> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if path.extension().is_some_and(|e| e == "toml") {
        toml::from_str(&raw).with_context(|| format!("parse {} as TOML", path.display()))
    } else {
        serde_json::from_str(&raw).with_context(|| format!("parse {} as JSON", path.display()))
    }
}

pub async fn run_playbook(args: &PlaybookArgs) -> Result<()> {
    let playbook = load_playbook(&args.file)?;
    if let Some(name) = &playbook.name {
        println!("playbook: {name}");
    }
    for (i, step) in playbook.step.iter().enumerate() {
        println!("-- step {i}: {}", step_name(step));
        if args.dry_run {
            continue;
        }
        run_step(step).await.with_context(|| format!("step {i}"))?;
    }
    Ok(())
}

fn step_name(step: &Step) -> &'static str {
    match step {
        Step::Exec(_) => "exec",
        Step::Shell(_) => "shell",
        Step::AssertFile(_) => "assert_file",
        Step::GitCommit(_) => "git_commit",
    }
}

async fn run_step(step: &Step) -> Result<()> {
    match step {
        Step::Exec(s) => run_exec(s).await,
        Step::Shell(s) => run_shell_step(&s.command, &s.args, s.expect_exit).await,
        Step::AssertFile(s) => run_assert_file(s),
        Step::GitCommit(s) => crate::git_commit_all(&s.message, false, None).await,
    }
}

async fn run_exec(step: &ExecStep) -> Result<()> {
    let session = SessionParams::default();
    run_single_turn_with(
        &step.prompt,
        step.model.clone(),
        step.yolo.unwrap_or(false),
        OutputFormat::Plain,
        step.max_turns,
        step.tools.clone(),
        None,
        None,
        None,
        &session,
        false,
    )
    .await?;
    Ok(())
}

pub(crate) fn resolve_shell_command(command: &str) -> Result<PathBuf> {
    if command.is_empty() {
        bail!("shell command is empty");
    }
    if command.contains('/') || command.contains('\\') {
        bail!("shell command must be a single executable name without path separators");
    }
    let filtered_path = match std::env::var_os("PATH") {
        Some(p) => {
            let paths: Vec<_> = std::env::split_paths(&p)
                .filter(|d| d.is_absolute())
                .collect();
            if paths.is_empty() {
                bail!("PATH contains no absolute directories");
            }
            std::env::join_paths(paths)
                .map_err(|_| anyhow::anyhow!("PATH contains invalid entries"))?
        }
        None => bail!("PATH is not set"),
    };
    let mut candidates = which::which_in_global(command, Some(&filtered_path))
        .with_context(|| format!("failed to resolve command {command}"))?;
    let resolved = candidates
        .next()
        .with_context(|| format!("command not found in PATH: {command}"))?;

    // Resolve symlinks and verify the real path is a regular file in a
    // directory not writable by group or others so a PATH/symlink swap cannot
    // redirect us to a malicious executable.
    let resolved = dunce::canonicalize(&resolved).with_context(|| {
        format!(
            "failed to canonicalize {resolved}",
            resolved = resolved.display()
        )
    })?;
    let meta = std::fs::symlink_metadata(&resolved)
        .with_context(|| format!("metadata for {resolved}", resolved = resolved.display()))?;
    if !meta.is_file() {
        bail!(
            "resolved command {resolved} is not a regular file",
            resolved = resolved.display()
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o022 != 0 {
            bail!(
                "resolved command {resolved} is writable by group or others",
                resolved = resolved.display()
            );
        }
        if let Some(parent) = resolved.parent() {
            let parent_meta = std::fs::symlink_metadata(parent)
                .with_context(|| format!("metadata for {parent}", parent = parent.display()))?;
            if !parent_meta.is_dir() {
                bail!(
                    "parent of {resolved} is not a directory",
                    resolved = resolved.display()
                );
            }
            let parent_mode = parent_meta.permissions().mode() & 0o777;
            if parent_mode & 0o022 != 0 {
                bail!(
                    "parent directory {parent} is writable by group or others",
                    parent = parent.display()
                );
            }
        }
    }

    Ok(resolved)
}

pub(crate) async fn run_shell_step(
    command: &str,
    args: &[String],
    expect_exit: Option<i32>,
) -> Result<()> {
    guard_shell_command(command, args).await?;
    let exe = resolve_shell_command(command)?;
    let mut cmd = Command::new(exe.as_os_str());
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (mut child, group) = crate::spawn_with_process_group(cmd)?;
    let mut stdout = child.stdout.take().context("stdout not piped")?;
    let mut stderr = child.stderr.take().context("stderr not piped")?;
    let mut out_capture = crate::BoundedCapture::new(MAX_CAPTURE_BYTES as usize);
    let mut err_capture = crate::BoundedCapture::new(MAX_CAPTURE_BYTES as usize);

    let out_handle = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stdout, &mut out_capture).await;
        out_capture.into_string()
    });
    let err_handle = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stderr, &mut err_capture).await;
        err_capture.into_string()
    });

    let status = match tokio::time::timeout(SHELL_STEP_TIMEOUT, child.wait()).await {
        Ok(s) => s?,
        Err(_) => {
            crate::kill_child_and_reap(&mut child, group.as_ref()).await;
            out_handle.abort();
            err_handle.abort();
            bail!(
                "shell step timed out after {}s",
                SHELL_STEP_TIMEOUT.as_secs()
            );
        }
    };
    crate::kill_process_group(group.as_ref());

    let out = out_handle.await.unwrap_or_default();
    let err = err_handle.await.unwrap_or_default();
    if !out.is_empty() {
        println!("{out}");
    }
    if let Some(expected) = expect_exit {
        let code = status.code().unwrap_or(-1);
        if code != expected {
            bail!("shell exited {code}, expected {expected}; stderr: {err}");
        }
    } else if !status.success() {
        bail!(
            "shell exited with {}; stderr: {err}",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

pub(crate) fn safe_shell_guard_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) {
        "safe-shell-guard.exe"
    } else {
        "safe-shell-guard"
    };
    if let Some(dir) = exe.parent() {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        let mut current = dir;
        while let Some(parent) = current.parent() {
            let candidate = parent.join("plugin").join("bin").join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
            current = parent;
        }
    }
    None
}

pub(crate) async fn guard_shell_command(command: &str, args: &[String]) -> Result<()> {
    let guard = safe_shell_guard_path()
        .ok_or_else(|| anyhow::anyhow!("safe-shell-guard binary not found; build it first"))?;
    let mut parts = vec![
        shlex::try_quote(command)
            .map_err(|_| anyhow::anyhow!("command contains invalid shell characters"))?
            .into_owned(),
    ];
    for a in args {
        parts.push(
            shlex::try_quote(a)
                .map_err(|_| anyhow::anyhow!("argument contains invalid shell characters"))?
                .into_owned(),
        );
    }
    let payload = json!({ "toolInput": { "command": parts.join(" ") } }).to_string();
    let mut child = Command::new(guard)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn safe-shell-guard")?;
    let mut stdin = child
        .stdin
        .take()
        .context("safe-shell-guard stdin not available")?;
    stdin.write_all(payload.as_bytes()).await?;
    stdin.shutdown().await?;
    let out = child
        .wait_with_output()
        .await
        .context("safe-shell-guard failed to run")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("safe-shell-guard output is not valid JSON: {e}"))?;
    if parsed.get("decision").and_then(|v| v.as_str()) != Some("allow") {
        let reason = parsed
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("blocked");
        bail!("shell command blocked by safe-shell-guard: {reason}");
    }
    Ok(())
}

fn run_assert_file(step: &AssertFileStep) -> Result<()> {
    if let Some(expected_exists) = step.exists {
        let actual = step.path.exists();
        if actual != expected_exists {
            bail!(
                "{} exists={actual}, expected={expected_exists}",
                step.path.display()
            );
        }
    }
    if let Some(needle) = &step.contains {
        let haystack = std::fs::read_to_string(&step.path)
            .with_context(|| format!("read {}", step.path.display()))?;
        if !haystack.contains(needle) {
            bail!("{} does not contain: {needle}", step.path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_playbook() {
        let raw = r#"{
            "name": "ci",
            "step": [
                { "type": "shell", "command": "echo", "args": ["ok"], "expect_exit": 0 },
                { "type": "assert_file", "path": "Cargo.toml", "exists": true }
            ]
        }"#;
        let pb: Playbook = serde_json::from_str(raw).unwrap();
        assert_eq!(pb.step.len(), 2);
    }

    #[test]
    fn parse_toml_playbook() {
        let raw = r#"
name = "ci"
[[step]]
type = "shell"
command = "echo"
args = ["ok"]
expect_exit = 0
[[step]]
type = "assert_file"
path = "Cargo.toml"
exists = true
"#;
        let pb: Playbook = toml::from_str(raw).unwrap();
        assert_eq!(pb.step.len(), 2);
    }

    #[test]
    fn assert_file_contains_fails_when_missing() {
        let tmp = std::env::temp_dir().join(format!("playbook-test-{}", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, "hello world").unwrap();
        let step = AssertFileStep {
            path: tmp.clone(),
            contains: Some("missing".into()),
            exists: None,
        };
        assert!(run_assert_file(&step).is_err());

        let step = AssertFileStep {
            path: tmp,
            contains: Some("hello".into()),
            exists: None,
        };
        assert!(run_assert_file(&step).is_ok());
    }

    #[test]
    fn assert_file_exists_check() {
        let missing =
            std::env::temp_dir().join(format!("playbook-missing-{}", uuid::Uuid::new_v4()));
        let step = AssertFileStep {
            path: missing,
            contains: None,
            exists: Some(true),
        };
        assert!(run_assert_file(&step).is_err());
    }

    #[test]
    fn resolve_shell_command_rejects_empty() {
        assert!(resolve_shell_command("").is_err());
    }

    #[test]
    fn resolve_shell_command_rejects_path_separators() {
        assert!(resolve_shell_command("foo/bar").is_err());
        assert!(resolve_shell_command("foo\\bar").is_err());
    }
}
