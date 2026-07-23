//! Session/job event timeline for `omgb`.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

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
    with_timeline_lock(true, || {
        let path = timeline_path()?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("timeline path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let event = TimelineEvent {
            timestamp: Utc::now(),
            category: category.into(),
            message: message.into(),
            data,
        };
        let line = serde_json::to_string(&event)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{line}")?;
        drop(file);
        crate::providers::restrict_omg_file_permissions(&path)?;
        Ok(())
    })
}

pub fn list_events(limit: usize, json: bool) -> Result<()> {
    with_timeline_lock(false, || {
        let path = timeline_path()?;
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path)?;
        let mut events: Vec<TimelineEvent> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| match serde_json::from_str(l) {
                Ok(e) => Some(e),
                Err(e) => {
                    eprintln!("warning: skipping malformed timeline line: {e}");
                    None
                }
            })
            .collect();
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
}
