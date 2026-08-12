//! Session/job event timeline for `omgb`.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

const MAX_TIMELINE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TIMELINE_EVENT_BYTES: usize = 1024 * 1024;
const MAX_TIMELINE_CATEGORY_BYTES: usize = 256;
const MAX_TIMELINE_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_TIMELINE_RESULTS: usize = 10_000;

fn timeline_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("timeline.jsonl"))
}

fn timeline_lock_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("timeline.jsonl.lock"))
}

fn with_timeline_lock<R>(exclusive: bool, f: impl FnOnce() -> Result<R>) -> Result<R> {
    let lock = timeline_lock_path()?;
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock)?;
    if exclusive {
        file.lock_exclusive()?;
    } else {
        file.lock_shared()?;
    }
    let result = f();
    let _ = file.unlock();
    result
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub timestamp: DateTime<Utc>,
    pub category: String,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

pub fn add_event(
    category: impl Into<String>,
    message: impl Into<String>,
    data: Option<serde_json::Value>,
) -> Result<()> {
    let category = category.into();
    let message = message.into();
    if category.trim().is_empty()
        || category.len() > MAX_TIMELINE_CATEGORY_BYTES
        || category.chars().any(char::is_control)
    {
        bail!(
            "timeline category must be non-empty, contain no controls, and be at most {MAX_TIMELINE_CATEGORY_BYTES} bytes"
        );
    }
    if message.trim().is_empty() || message.len() > MAX_TIMELINE_MESSAGE_BYTES {
        bail!("timeline message must be non-empty and at most {MAX_TIMELINE_MESSAGE_BYTES} bytes");
    }
    if message.contains('\0') {
        bail!("timeline message must not contain NUL characters");
    }
    with_timeline_lock(true, || {
        let path = timeline_path()?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("timeline path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let event = TimelineEvent {
            timestamp: Utc::now(),
            category,
            message,
            data,
        };
        let line = serde_json::to_string(&event)?;
        if line.len() > MAX_TIMELINE_EVENT_BYTES {
            bail!("timeline event is too large (max {MAX_TIMELINE_EVENT_BYTES} bytes)");
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&path)
            && !metadata.file_type().is_file()
        {
            bail!("timeline store is not a regular file: {}", path.display());
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let size = file.metadata()?.len();
        if size.saturating_add(line.len() as u64 + 1) > MAX_TIMELINE_BYTES {
            bail!("timeline store exceeds the {MAX_TIMELINE_BYTES} byte safety limit");
        }
        crate::providers::restrict_omg_file_permissions(&path)?;
        writeln!(file, "{line}")?;
        file.sync_data()?;
        Ok(())
    })
}

fn parse_events(path: &Path, raw: &str) -> Result<Vec<TimelineEvent>> {
    raw.lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).with_context(|| {
                format!(
                    "invalid JSON record in timeline store {} at line {}",
                    path.display(),
                    index + 1
                )
            })
        })
        .collect()
}

pub fn list_events(limit: usize, json: bool) -> Result<()> {
    if limit > MAX_TIMELINE_RESULTS {
        bail!("timeline result limit is too large (max {MAX_TIMELINE_RESULTS})");
    }
    with_timeline_lock(false, || {
        let path = timeline_path()?;
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => bail!("timeline store is not a regular file: {}", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("inspect timeline store"),
        };
        if metadata.len() > MAX_TIMELINE_BYTES {
            bail!("timeline store exceeds the {MAX_TIMELINE_BYTES} byte safety limit");
        }
        let raw = std::fs::read_to_string(&path)?;
        let mut events = parse_events(&path, &raw)?;
        events.sort_by_key(|b| std::cmp::Reverse(b.timestamp));
        let events = events.into_iter().take(limit);

        if json {
            let collected: Vec<_> = events.collect();
            println!("{}", serde_json::to_string_pretty(&collected)?);
        } else {
            for ev in events {
                println!(
                    "{} [{}] {}",
                    ev.timestamp.to_rfc3339(),
                    ev.category,
                    ev.message
                );
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeline_event_serialization() {
        let event = TimelineEvent {
            timestamp: DateTime::UNIX_EPOCH,
            category: "test".into(),
            message: "message".into(),
            data: Some(serde_json::json!({"k": "v"})),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: TimelineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.category, "test");
        assert_eq!(parsed.message, "message");
        assert!(parsed.data.is_some());
    }

    #[test]
    fn malformed_timeline_jsonl_fails_closed_with_line_number() {
        let raw = concat!(
            "{\"timestamp\":\"1970-01-01T00:00:00Z\",\"category\":\"ok\",\"message\":\"keep\",\"data\":null}\n",
            "{broken}\n"
        );
        let error = parse_events(Path::new("timeline.jsonl"), raw)
            .unwrap_err()
            .to_string();
        assert!(error.contains("line 2"), "unexpected error: {error}");
    }
}
