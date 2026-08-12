//! Persistent session list / resume / fork helpers.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
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
    Ok(sessions_root_from(&xai_grok_config::grok_home(), &cwd))
}

fn sessions_root_from(grok_home: &Path, cwd: &Path) -> PathBuf {
    let encoded = xai_grok_config::encode_cwd_dirname(&cwd.to_string_lossy());
    grok_home.join("sessions").join(encoded)
}

fn normalize_session_id(id: &str) -> Result<String> {
    uuid::Uuid::try_parse(id)
        .map(|id| id.to_string())
        .map_err(|_| anyhow::anyhow!("session id must be a valid UUID (got '{id}')"))
}

fn session_dir(id: &str) -> Result<PathBuf> {
    session_dir_from(&sessions_root()?, id)
}

fn session_dir_from(root: &Path, id: &str) -> Result<PathBuf> {
    Ok(root.join(normalize_session_id(id)?))
}

fn list_session_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(dirs),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let p = entry.path();
        let metadata = std::fs::symlink_metadata(&p)?;
        if metadata.file_type().is_symlink() {
            bail!("session directory entry is a symlink: {}", p.display());
        }
        if metadata.is_dir() {
            dirs.push(p);
        }
    }
    Ok(dirs)
}

fn read_summary(path: &Path) -> Result<SessionSummary> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("session summary is not a regular file: {}", path.display());
    }
    Ok(serde_json::from_str::<SessionSummary>(
        &std::fs::read_to_string(path)?,
    )?)
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
    let dirs = list_session_dirs(&root)?;
    if dirs.is_empty() {
        println!("No sessions found for this workspace.");
        return Ok(());
    }

    let mut sessions: Vec<(PathBuf, SessionSummary)> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let summary_path = dir.join("summary.json");
        let summary = match read_summary(&summary_path) {
            Ok(summary) => summary,
            Err(error) => {
                eprintln!(
                    "warning: session {} has an unreadable summary: {error}",
                    dir.file_name().unwrap_or_default().to_string_lossy()
                );
                SessionSummary {
                    summary: "[summary unavailable]".into(),
                    ..Default::default()
                }
            }
        };
        sessions.push((dir, summary));
    }

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
    let session_id = args
        .session_id
        .as_deref()
        .map(normalize_session_id)
        .transpose()?;
    let explicit_dir = session_id.as_deref().map(session_dir).transpose()?;
    if let Some(ref dir) = explicit_dir
        && dir.exists()
    {
        bail!(
            "session already exists: {}",
            args.session_id.as_deref().unwrap_or_default()
        );
    }
    let session = SessionParams {
        session_id,
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
    if args.source_session_id.is_some() && args.continue_last {
        bail!("pass a source session id or --continue, not both");
    }
    if args.target_session_id.is_some() && !args.fork_session {
        bail!("--session-id requires --fork-session when resuming");
    }
    let source_session_id = args
        .source_session_id
        .as_deref()
        .map(normalize_session_id)
        .transpose()?;
    let target_session_id = args
        .target_session_id
        .as_deref()
        .map(normalize_session_id)
        .transpose()?;
    if let Some(ref sid) = source_session_id
        && !session_dir(sid)?.is_dir()
    {
        bail!("source session '{sid}' does not exist in this workspace");
    }
    if let Some(ref sid) = target_session_id
        && session_dir(sid)?.exists()
    {
        bail!("target session '{sid}' already exists");
    }
    let session = SessionParams {
        resume: source_session_id,
        session_id: target_session_id,
        fork_session: args.fork_session,
        continue_last: args.continue_last,
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
    let parent = normalize_session_id(&args.parent_session_id)?;
    let parent_dir = session_dir(&parent)?;
    if !parent_dir.is_dir() {
        bail!("parent session '{}' does not exist", parent);
    }
    let parent_meta = std::fs::symlink_metadata(&parent_dir)?;
    if parent_meta.is_symlink() {
        bail!("parent session '{}' is a symbolic link", parent);
    }

    let new_id = match args.new_session_id {
        Some(id) => normalize_session_id(&id)?,
        None => uuid::Uuid::new_v4().to_string(),
    };
    let new_dir = session_dir(&new_id)?;
    if new_dir.exists() {
        bail!("session '{new_id}' already exists");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_paths_under_grok_home() {
        let tmp = std::env::temp_dir().join(format!("omgb-session-test-{}-", uuid::Uuid::new_v4()));
        let cwd = tmp.join("workspace");

        let root = sessions_root_from(&tmp, &cwd);
        assert!(root.starts_with(&tmp));
        assert!(root.to_string_lossy().contains("sessions"));

        let id = uuid::Uuid::new_v4().to_string();
        let dir = session_dir_from(&root, &id).unwrap();
        assert!(dir.starts_with(&tmp));
        assert_eq!(dir.file_name().unwrap_or_default(), id.as_str());
    }

    #[test]
    fn session_ids_match_the_upstream_uuid_contract() {
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(normalize_session_id(&id).unwrap(), id);
        assert!(normalize_session_id("named-session").is_err());
        assert!(normalize_session_id("../escape").is_err());
    }
}
