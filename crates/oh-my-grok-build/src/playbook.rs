//! Deterministic CI-style playbooks for `omgb`.
//!
//! Playbooks are TOML or JSON files describing a sequence of steps that can be
//! run non-interactively. They are intended for headless / CI use where the
//! model, tools, and expected outcomes are pinned.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

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
        Step::Shell(s) => run_shell(s).await,
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

async fn run_shell(step: &ShellStep) -> Result<()> {
    let mut cmd = Command::new(&step.command);
    cmd.args(&step.args);
    let status = cmd.status().await?;
    if let Some(expected) = step.expect_exit {
        let code = status.code().unwrap_or(-1);
        if code != expected {
            bail!("shell exited {code}, expected {expected}");
        }
    } else if !status.success() {
        bail!("shell exited with {}", status.code().unwrap_or(-1));
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
}
