//! Meta-harness notification log.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

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

pub fn push(event_type: &str, data: serde_json::Value) -> Result<()> {
    let path = notifications_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("notifications path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let n = Notification {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        timestamp: Utc::now(),
        data,
    };
    let line = serde_json::to_string(&n)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)?;
    file.lock_exclusive()?;
    writeln!(file, "{line}")?;
    drop(file);
    crate::providers::restrict_omg_file_permissions(&path)?;
    Ok(())
}

pub fn list(limit: usize) -> Result<Vec<Notification>> {
    let path = notifications_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut file =
        std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
    file.lock_shared()?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)?;
    drop(file);
    let mut notifs: Vec<Notification> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
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
        unsafe { std::env::set_var("OMGB_HOME", home.as_os_str()) };

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
        unsafe { std::env::remove_var("OMGB_HOME") };
    }
}
