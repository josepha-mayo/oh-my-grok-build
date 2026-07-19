//! Taste/preference learning store for `omgb`.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

fn taste_path() -> PathBuf {
    crate::providers::omg_dir().join("taste.json")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TasteStore {
    pub likes: Vec<TasteNote>,
    pub dislikes: Vec<TasteNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasteNote {
    pub timestamp: DateTime<Utc>,
    pub note: String,
}

fn load_store() -> Result<TasteStore> {
    let path = taste_path();
    if !path.exists() {
        return Ok(TasteStore::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn save_store(store: &TasteStore) -> Result<()> {
    let path = taste_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(store)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn add_like(note: &str) -> Result<()> {
    let mut store = load_store()?;
    store.likes.push(TasteNote {
        timestamp: Utc::now(),
        note: note.to_string(),
    });
    save_store(&store)
}

pub fn add_dislike(note: &str) -> Result<()> {
    let mut store = load_store()?;
    store.dislikes.push(TasteNote {
        timestamp: Utc::now(),
        note: note.to_string(),
    });
    save_store(&store)
}

pub fn list_taste() -> Result<()> {
    let store = load_store()?;
    if store.likes.is_empty() && store.dislikes.is_empty() {
        println!("No taste preferences recorded yet.");
        return Ok(());
    }
    if !store.likes.is_empty() {
        println!("Preferences (like):");
        for n in &store.likes {
            println!("  - [{}] {}", n.timestamp.to_rfc3339(), n.note);
        }
    }
    if !store.dislikes.is_empty() {
        println!("Avoid (dislike):");
        for n in &store.dislikes {
            println!("  - [{}] {}", n.timestamp.to_rfc3339(), n.note);
        }
    }
    Ok(())
}

/// Returns a short prompt preamble derived from stored preferences.
pub fn taste_preamble() -> String {
    let Ok(store) = load_store() else {
        return String::new();
    };
    let mut parts = Vec::new();
    for n in &store.likes {
        parts.push(format!("Prefer: {}", n.note));
    }
    for n in &store.dislikes {
        parts.push(format!("Avoid: {}", n.note));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("\n\nTaste profile:\n{}", parts.join("\n"))
    }
}
