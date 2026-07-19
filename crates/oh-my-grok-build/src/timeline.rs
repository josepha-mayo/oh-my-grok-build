//! Session/job event timeline for `omgb`.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

fn timeline_path() -> PathBuf {
    crate::providers::omg_dir().join("timeline.jsonl")
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
    let path = timeline_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
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
    Ok(())
}

pub fn list_events(limit: usize, json: bool) -> Result<()> {
    let path = timeline_path();
    if !path.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(&path)?;
    let mut events: Vec<TimelineEvent> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    events.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
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
}
