//! Taste/preference learning store for `omgb`.

use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

fn taste_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("taste.json"))
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
    let path = taste_path()?;
    if !path.exists() {
        return Ok(TasteStore::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

fn save_store(store: &TasteStore) -> Result<()> {
    let path = taste_path()?;
    crate::providers::write_file_atomic(&path, serde_json::to_string_pretty(store)?, true)
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
    for n in store.likes.iter().rev().take(10) {
        parts.push(format!("Prefer: {}", n.note));
    }
    for n in store.dislikes.iter().rev().take(10) {
        parts.push(format!("Avoid: {}", n.note));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("\n\nTaste profile:\n{}", parts.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_taste_store_serialization() {
        let store = TasteStore {
            likes: vec![TasteNote {
                timestamp: DateTime::UNIX_EPOCH,
                note: "compact code".into(),
            }],
            dislikes: vec![TasteNote {
                timestamp: DateTime::UNIX_EPOCH,
                note: "verbose error handling".into(),
            }],
        };
        let json = serde_json::to_string(&store).unwrap();
        let parsed: TasteStore = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.likes.len(), 1);
        assert_eq!(parsed.likes[0].note, "compact code");
        assert_eq!(parsed.dislikes.len(), 1);
    }

    #[test]
    fn test_taste_preamble_format() {
        let store = TasteStore {
            likes: vec![TasteNote {
                timestamp: DateTime::UNIX_EPOCH,
                note: "tests".into(),
            }],
            dislikes: vec![],
        };
        let json = serde_json::to_string(&store).unwrap();
        // taste_preamble reads from disk, so this only checks the store shape.
        assert!(json.contains("tests"));
    }
}
