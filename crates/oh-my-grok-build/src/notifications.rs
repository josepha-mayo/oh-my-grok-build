//! Meta-harness notification log.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

const MAX_NOTIFICATIONS_BYTES: u64 = 64 * 1024 * 1024;
const MAX_NOTIFICATION_BYTES: usize = 1024 * 1024;
const MAX_NOTIFICATION_TYPE_BYTES: usize = 256;
const MAX_NOTIFICATION_RESULTS: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub event_type: String,
    pub timestamp: DateTime<Utc>,
    pub data: serde_json::Value,
}

fn notifications_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?
        .join("meta")
        .join("notifications.jsonl"))
}

fn notifications_lock_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?
        .join("meta")
        .join("notifications.lock"))
}

fn notification_lock(exclusive: bool) -> Result<std::fs::File> {
    let path = notifications_lock_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("notifications lock has no parent"))?;
    std::fs::create_dir_all(parent)?;
    crate::providers::restrict_omg_directory_permissions(parent)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    crate::providers::restrict_omg_file_permissions(&path)?;
    if exclusive {
        file.lock_exclusive()?;
    } else {
        file.lock_shared()?;
    }
    Ok(file)
}

pub fn push(event_type: &str, data: serde_json::Value) -> Result<()> {
    if event_type.trim().is_empty()
        || event_type.len() > MAX_NOTIFICATION_TYPE_BYTES
        || event_type.chars().any(char::is_control)
    {
        bail!(
            "notification event type must be non-empty, contain no controls, and be at most {MAX_NOTIFICATION_TYPE_BYTES} bytes"
        );
    }
    let path = notifications_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("notifications path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    crate::providers::restrict_omg_directory_permissions(parent)?;
    let _lock = notification_lock(true)?;
    let n = Notification {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        timestamp: Utc::now(),
        data,
    };
    let line = serde_json::to_string(&n)?;
    if line.len() > MAX_NOTIFICATION_BYTES {
        bail!("notification is too large (max {MAX_NOTIFICATION_BYTES} bytes)");
    }
    if let Ok(metadata) = std::fs::symlink_metadata(&path)
        && !metadata.file_type().is_file()
    {
        bail!(
            "notifications store is not a regular file: {}",
            path.display()
        );
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)?;
    crate::providers::restrict_omg_file_permissions(&path)?;
    let size = file.metadata()?.len();
    if size.saturating_add(line.len() as u64 + 1) > MAX_NOTIFICATIONS_BYTES {
        let raw = std::fs::read_to_string(&path)?;
        let mut kept = Vec::new();
        let mut bytes = line.len() + 1;
        let existing_lines: Vec<_> = raw.lines().collect();
        for index in (0..existing_lines.len()).rev() {
            let existing = existing_lines[index];
            let _: Notification = serde_json::from_str(existing).with_context(|| {
                format!(
                    "invalid JSON record in notifications store at line {}",
                    index + 1
                )
            })?;
            if bytes + existing.len() + 1 > (MAX_NOTIFICATIONS_BYTES / 2) as usize {
                break;
            }
            bytes += existing.len() + 1;
            kept.push(existing);
        }
        kept.reverse();
        let mut compacted = kept.join("\n");
        if !compacted.is_empty() {
            compacted.push('\n');
        }
        compacted.push_str(&line);
        compacted.push('\n');
        drop(file);
        return crate::providers::write_file_atomic(&path, compacted, true);
    }
    writeln!(file, "{line}")?;
    file.sync_data()?;
    Ok(())
}

pub fn list(limit: usize) -> Result<Vec<Notification>> {
    if limit > MAX_NOTIFICATION_RESULTS {
        bail!("notification result limit is too large (max {MAX_NOTIFICATION_RESULTS})");
    }
    let path = notifications_path()?;
    let _lock = notification_lock(false)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => bail!(
            "notifications store is not a regular file: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("inspect notifications store"),
    };
    if metadata.len() > MAX_NOTIFICATIONS_BYTES {
        bail!("notifications store exceeds the {MAX_NOTIFICATIONS_BYTES} byte safety limit");
    }
    let mut file =
        std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)?;
    drop(file);
    let mut notifs: Vec<Notification> = raw
        .lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).with_context(|| {
                format!(
                    "invalid JSON record in notifications store {} at line {}",
                    path.display(),
                    index + 1
                )
            })
        })
        .collect::<Result<_>>()?;
    notifs.reverse();
    Ok(notifs.into_iter().take(limit).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("omgb-notifications-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn test_push_and_list() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));

        push("event1", serde_json::json!({"k": "v1"})).unwrap();
        push("event2", serde_json::json!({"k": "v2"})).unwrap();

        let notifs = list(10).unwrap();
        assert_eq!(notifs.len(), 2);
        assert_eq!(notifs[0].event_type, "event2");
        assert_eq!(notifs[1].event_type, "event1");
        assert_eq!(notifs[0].data["k"].as_str(), Some("v2"));
        assert_eq!(notifs[1].data["k"].as_str(), Some("v1"));

        let limited = list(1).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].event_type, "event2");

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn malformed_notifications_jsonl_fails_closed_with_line_number() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        let meta = home.join("meta");
        std::fs::create_dir_all(&meta).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let path = meta.join("notifications.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"id\":\"1\",\"event_type\":\"ok\",\"timestamp\":\"1970-01-01T00:00:00Z\",\"data\":null}\n",
                "{broken}\n"
            ),
        )
        .unwrap();

        let error = list(10).unwrap_err().to_string();
        assert!(error.contains("line 2"), "unexpected error: {error}");

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }
}
