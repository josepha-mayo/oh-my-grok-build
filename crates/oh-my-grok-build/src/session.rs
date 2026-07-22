//! Persistent session list / resume / fork helpers.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use serde::Deserialize;

use crate::args::{
    SessionCommand, SessionForkArgs, SessionNewArgs, SessionParams, SessionResumeArgs,
};
use crate::run_single_turn_with;
use xai_grok_pager::headless::OutputFormat;

#[derive(Debug, Deserialize, Default)]
struct SessionSummary {
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    start_time: Option<i64>,
    #[serde(default)]
    last_message_time: Option<i64>,
}

fn sessions_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let encoded = xai_grok_config::encode_cwd_dirname(&cwd.to_string_lossy());
    Ok(xai_grok_config::grok_home().join("sessions").join(encoded))
}

fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn validate_session_id(id: &str) -> Result<()> {
    if !is_safe_session_id(id) {
        bail!("invalid session id '{id}'");
    }
    Ok(())
}

fn session_dir(id: &str) -> Result<PathBuf> {
    validate_session_id(id)?;
    Ok(sessions_root()?.join(id))
}

fn list_session_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                dirs.push(p);
            }
        }
    }
    dirs
}

fn read_summary(path: &Path) -> Option<SessionSummary> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<SessionSummary>(&s).ok())
}

fn fmt_time(ts: Option<i64>) -> String {
    ts.and_then(|t| DateTime::from_timestamp(t, 0))
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "-".to_string())
}

pub async fn run_session(cmd: SessionCommand) -> Result<()> {
    match cmd {
        SessionCommand::List => list_sessions(),
        SessionCommand::New(args) => run_session_new(args).await,
        SessionCommand::Resume(args) => run_session_resume(args).await,
        SessionCommand::Fork(args) => run_session_fork(args).await,
    }
}

fn list_sessions() -> Result<()> {
    let root = sessions_root()?;
    let dirs = list_session_dirs(&root);
    if dirs.is_empty() {
        println!("No sessions found for this workspace.");
        return Ok(());
    }

    let mut sessions: Vec<(PathBuf, SessionSummary)> = dirs
        .into_iter()
        .filter_map(|d| {
            let summary = read_summary(&d.join("summary.json"))?;
            Some((d, summary))
        })
        .collect();

    sessions.sort_by(|a, b| b.1.last_message_time.cmp(&a.1.last_message_time));

    for (dir, summary) in sessions {
        let id = dir.file_name().unwrap_or_default().to_string_lossy();
        println!(
            "{}\n  cwd: {}\n  summary: {}\n  start: {}  last: {}",
            id,
            summary.cwd,
            summary.summary.lines().next().unwrap_or(""),
            fmt_time(summary.start_time),
            fmt_time(summary.last_message_time)
        );
    }
    Ok(())
}

async fn run_session_new(args: SessionNewArgs) -> Result<()> {
    if let Some(ref sid) = args.session_id
        && session_dir(sid)?.exists()
    {
        bail!("session already exists: {sid}");
    }
    let session = SessionParams {
        session_id: args.session_id,
        ..Default::default()
    };
    run_single_turn_with(
        &args.prompt,
        args.model,
        args.yolo,
        OutputFormat::Plain,
        None,
        None,
        None,
        None,
        None,
        &session,
        args.memory,
    )
    .await
}

async fn run_session_resume(args: SessionResumeArgs) -> Result<()> {
    if let Some(ref sid) = args.source_session_id {
        validate_session_id(sid)?;
    }
    if let Some(ref sid) = args.target_session_id {
        validate_session_id(sid)?;
    }
    let resume = if args.continue_last {
        Some(String::new())
    } else {
        args.source_session_id.clone()
    };
    let session = SessionParams {
        resume,
        session_id: args.target_session_id,
        fork_session: args.fork_session,
        continue_last: false,
    };
    let prompt = args.prompt.unwrap_or_default();
    run_single_turn_with(
        &prompt,
        args.model,
        args.yolo,
        OutputFormat::Plain,
        None,
        None,
        None,
        None,
        None,
        &session,
        args.memory,
    )
    .await
}

async fn run_session_fork(args: SessionForkArgs) -> Result<()> {
    let parent = args.parent_session_id;
    validate_session_id(&parent)?;
    let parent_dir = session_dir(&parent)?;
    if !parent_dir.exists() {
        bail!("parent session '{}' does not exist", parent);
    }

    let new_id = match args.new_session_id {
        Some(id) => {
            validate_session_id(&id)?;
            id
        }
        None => {
            let id = format!("{parent}-fork-{}", uuid::Uuid::new_v4());
            validate_session_id(&id)?;
            id
        }
    };
    let new_dir = session_dir(&new_id)?;
    if new_dir.exists() {
        bail!("session '{new_id}' already exists");
    }
    std::fs::create_dir_all(&new_dir)?;
    copy_compaction_checkpoints(&parent_dir, &new_dir)
        .with_context(|| "failed to copy compaction checkpoints")?;

    let session = SessionParams {
        resume: Some(parent),
        session_id: Some(new_id),
        fork_session: true,
        continue_last: false,
    };
    let prompt = args.prompt.unwrap_or_default();
    run_single_turn_with(
        &prompt,
        args.model,
        args.yolo,
        OutputFormat::Plain,
        None,
        None,
        None,
        None,
        None,
        &session,
        args.memory,
    )
    .await
}

fn copy_compaction_checkpoints(src: &Path, dst: &Path) -> Result<()> {
    let src_ckpt = src.join("compaction_checkpoints");
    if !src_ckpt.exists() {
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(&src_ckpt)
        .with_context(|| format!("metadata for {}", src_ckpt.display()))?;
    if !meta.is_dir() {
        bail!("{} is not a directory", src_ckpt.display());
    }
    let dst_ckpt = dst.join("compaction_checkpoints");
    std::fs::create_dir_all(&dst_ckpt)?;
    for entry in std::fs::read_dir(&src_ckpt)? {
        let entry = entry?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .with_context(|| format!("metadata for {}", path.display()))?;
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_compaction_checkpoints(&path, &dst_ckpt.join(entry.file_name()))?;
        } else if meta.is_file() {
            std::fs::copy(&path, dst_ckpt.join(entry.file_name()))
                .with_context(|| format!("copy {} to {}", path.display(), dst_ckpt.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_paths_under_grok_home() {
        let tmp = std::env::temp_dir();
        unsafe { std::env::set_var("GROK_HOME", tmp.as_os_str()) };
        let root = sessions_root().unwrap();
        assert!(root.to_string_lossy().contains("sessions"));
        let dir = session_dir("sess-1").unwrap();
        assert!(dir.to_string_lossy().contains("sess-1"));
        unsafe { std::env::remove_var("GROK_HOME") };
    }
}
