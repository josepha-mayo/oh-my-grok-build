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
    pub accepted: Vec<TasteOutput>,
    pub rejected: Vec<TasteOutput>,
    pub edited: Vec<TasteEdit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasteNote {
    pub timestamp: DateTime<Utc>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasteOutput {
    pub timestamp: DateTime<Utc>,
    pub topic: String,
    pub output: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasteEdit {
    pub timestamp: DateTime<Utc>,
    pub topic: String,
    pub before: String,
    pub after: String,
    pub tags: Vec<String>,
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

fn topic_from_prompt(prompt: &str) -> String {
    let head = prompt.lines().next().unwrap_or(prompt).trim();
    if head.is_empty() {
        return "untitled".into();
    }
    if head.len() <= 80 {
        head.into()
    } else {
        format!(
            "{}...",
            &head[..head.char_indices().nth(77).map(|(i, _)| i).unwrap_or(77)]
        )
    }
}

fn snippet(text: &str) -> String {
    if text.len() <= 80 {
        text.into()
    } else {
        let end = text.char_indices().nth(77).map(|(i, _)| i).unwrap_or(77);
        format!("{}...", &text[..end])
    }
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

pub fn taste_accept(prompt: &str, output: &str, tags: Vec<String>) -> Result<()> {
    let mut store = load_store()?;
    store.accepted.push(TasteOutput {
        timestamp: Utc::now(),
        topic: topic_from_prompt(prompt),
        output: output.to_string(),
        tags,
    });
    save_store(&store)
}

pub fn taste_reject(prompt: &str, output: &str, tags: Vec<String>) -> Result<()> {
    let mut store = load_store()?;
    store.rejected.push(TasteOutput {
        timestamp: Utc::now(),
        topic: topic_from_prompt(prompt),
        output: output.to_string(),
        tags,
    });
    save_store(&store)
}

pub fn taste_edit(prompt: &str, before: &str, after: &str, tags: Vec<String>) -> Result<()> {
    let mut store = load_store()?;
    store.edited.push(TasteEdit {
        timestamp: Utc::now(),
        topic: topic_from_prompt(prompt),
        before: before.to_string(),
        after: after.to_string(),
        tags,
    });
    save_store(&store)
}

pub fn list_taste() -> Result<()> {
    let store = load_store()?;
    if store.likes.is_empty()
        && store.dislikes.is_empty()
        && store.accepted.is_empty()
        && store.rejected.is_empty()
        && store.edited.is_empty()
    {
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
    if !store.accepted.is_empty() {
        println!("Accepted outputs:");
        for e in &store.accepted {
            println!(
                "  - [{}] {} | tags: {:?}",
                e.timestamp.to_rfc3339(),
                e.topic,
                e.tags
            );
        }
    }
    if !store.rejected.is_empty() {
        println!("Rejected outputs:");
        for e in &store.rejected {
            println!(
                "  - [{}] {} | tags: {:?}",
                e.timestamp.to_rfc3339(),
                e.topic,
                e.tags
            );
        }
    }
    if !store.edited.is_empty() {
        println!("Edited outputs:");
        for e in &store.edited {
            println!(
                "  - [{}] {} | tags: {:?}",
                e.timestamp.to_rfc3339(),
                e.topic,
                e.tags
            );
        }
    }
    Ok(())
}

fn build_style_rules(store: &TasteStore) -> Vec<String> {
    let mut rules = Vec::new();

    for n in store.likes.iter().rev().take(5) {
        rules.push(format!("Prefer {}", n.note));
    }
    for n in store.dislikes.iter().rev().take(5) {
        rules.push(format!("Avoid {}", n.note));
    }
    for e in store.accepted.iter().rev().take(5) {
        rules.push(format!("Always do {}: {}", e.topic, snippet(&e.output)));
    }
    for e in store.rejected.iter().rev().take(5) {
        rules.push(format!("Avoid {}: {}", e.topic, snippet(&e.output)));
    }
    for e in store.edited.iter().rev().take(5) {
        rules.push(format!(
            "Prefer {} over {} for {}",
            snippet(&e.after),
            snippet(&e.before),
            e.topic
        ));
    }

    rules
}

/// Returns a short prompt preamble derived from stored preferences.
pub fn taste_preamble() -> String {
    let Ok(store) = load_store() else {
        return String::new();
    };
    let rules = build_style_rules(&store);
    if rules.is_empty() {
        String::new()
    } else {
        format!("\n\nTaste profile: {}", rules.join("; "))
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
            ..Default::default()
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
            ..Default::default()
        };
        let rules = build_style_rules(&store);
        assert!(rules.iter().any(|r| r.contains("Prefer tests")));
    }

    #[test]
    fn test_taste_accept_reject_edit_rules() {
        let store = TasteStore {
            accepted: vec![TasteOutput {
                timestamp: DateTime::UNIX_EPOCH,
                topic: "rust".into(),
                output: "use Result".into(),
                tags: vec!["style".into()],
            }],
            rejected: vec![TasteOutput {
                timestamp: DateTime::UNIX_EPOCH,
                topic: "rust".into(),
                output: "unwrap everywhere".into(),
                tags: vec!["style".into()],
            }],
            edited: vec![TasteEdit {
                timestamp: DateTime::UNIX_EPOCH,
                topic: "rust".into(),
                before: "verbose".into(),
                after: "compact".into(),
                tags: vec!["style".into()],
            }],
            ..Default::default()
        };
        let rules = build_style_rules(&store);
        assert!(rules.iter().any(|r| r.contains("Always do")));
        assert!(rules.iter().any(|r| r.contains("Avoid")));
        assert!(
            rules
                .iter()
                .any(|r| r.contains("Prefer compact over verbose"))
        );
    }
}
