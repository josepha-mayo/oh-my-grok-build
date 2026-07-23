//! Persistent cross-session memory store (JSONL-backed) for omgb.
//!
//! Notes are stored in `~/.omgb/memory.jsonl` with a small keyword search
//! index rebuilt on first access each run.  This is intentionally simple so
//! it works without embedding providers or a build dependency on sqlite-vec.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::args::MemoryCommand;

const MEMORY_FILE: &str = "memory.jsonl";
const ONE_SHOT_FILE: &str = "one_shot_journal.jsonl";

fn memory_path() -> Result<std::path::PathBuf> {
    Ok(crate::providers::omg_dir()?.join(MEMORY_FILE))
}

fn ensure_store() -> Result<()> {
    let dir = crate::providers::omg_dir()?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }
    let path = dir.join(MEMORY_FILE);
    if !path.exists() {
        std::fs::File::create(&path)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNote {
    pub id: String,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub content: String,
    #[serde(default)]
    pub access_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OneShotNote {
    pub id: String,
    pub created_at: i64,
    pub topic: String,
    pub detail: String,
    #[serde(default)]
    pub seen: bool,
}

fn load_notes() -> Result<Vec<MemoryNote>> {
    ensure_store()?;
    let path = memory_path()?;
    if path.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        return Ok(Vec::new());
    }
    let mut notes = Vec::new();
    for line in std::fs::read_to_string(&path)?.lines() {
        if let Ok(n) = serde_json::from_str::<MemoryNote>(line) {
            notes.push(n);
        }
    }
    Ok(notes)
}

fn save_notes(notes: &[MemoryNote]) -> Result<()> {
    ensure_store()?;
    let path = memory_path()?;
    let mut content = String::new();
    for note in notes {
        content.push_str(&serde_json::to_string(note)?);
        content.push('\n');
    }
    crate::providers::write_file_atomic(&path, content, true)
}

fn one_shot_path() -> Result<std::path::PathBuf> {
    Ok(crate::providers::omg_dir()?.join(ONE_SHOT_FILE))
}

fn load_one_shots() -> Result<Vec<OneShotNote>> {
    let path = one_shot_path()?;
    if !path.exists() || path.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        return Ok(Vec::new());
    }
    let mut notes = Vec::new();
    for line in std::fs::read_to_string(&path)?.lines() {
        if let Ok(n) = serde_json::from_str::<OneShotNote>(line) {
            notes.push(n);
        }
    }
    Ok(notes)
}

fn save_one_shots(notes: &[OneShotNote]) -> Result<()> {
    let path = one_shot_path()?;
    let mut content = String::new();
    for note in notes {
        content.push_str(&serde_json::to_string(note)?);
        content.push('\n');
    }
    crate::providers::write_file_atomic(&path, content, true)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn tokenize(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|c: char| c.is_whitespace() || c == ',' || c == '.' || c == ';' || c == ':')
        .filter(|s| !s.is_empty() && s.len() > 2)
        .map(|s| s.to_string())
        .collect()
}

fn score_note(note: &MemoryNote, terms: &[String]) -> usize {
    let text = format!("{} {}", note.content, note.tags.join(" ")).to_lowercase();
    terms.iter().filter(|t| text.contains(t.as_str())).count()
}

fn score_one_shot(note: &OneShotNote, terms: &[String]) -> usize {
    let text = format!("{} {}", note.topic, note.detail).to_lowercase();
    terms.iter().filter(|t| text.contains(t.as_str())).count()
}

pub fn remember(content: &str, tags: &[String]) -> Result<MemoryNote> {
    let mut notes = load_notes()?;
    let note = MemoryNote {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: now(),
        tags: tags.to_vec(),
        content: content.to_string(),
        access_count: 0,
    };
    notes.push(note.clone());
    save_notes(&notes)?;
    Ok(note)
}

pub fn remember_one_shot(topic: &str, detail: &str) -> Result<OneShotNote> {
    let mut notes = load_one_shots()?;
    let note = OneShotNote {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: now(),
        topic: topic.to_string(),
        detail: detail.to_string(),
        seen: false,
    };
    notes.push(note.clone());
    save_one_shots(&notes)?;
    Ok(note)
}

pub fn list(tag: Option<&str>, limit: usize) -> Result<Vec<MemoryNote>> {
    let mut notes = load_notes()?;
    notes.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(notes
        .into_iter()
        .filter(|n| tag.is_none_or(|t| n.tags.iter().any(|x| x == t)))
        .take(limit)
        .collect())
}

pub fn recall(query: &str, limit: usize) -> Result<Vec<MemoryNote>> {
    let terms = tokenize(query);
    if terms.is_empty() {
        return list(None, limit);
    }
    let notes = load_notes()?;
    let mut scored: Vec<(usize, MemoryNote)> = notes
        .into_iter()
        .filter_map(|n| {
            let score = score_note(&n, &terms);
            if score > 0 { Some((score, n)) } else { None }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.created_at.cmp(&a.1.created_at))
    });
    Ok(scored.into_iter().map(|(_, n)| n).take(limit).collect())
}

pub fn recall_one_shot(topic: &str, n: usize) -> Result<Vec<OneShotNote>> {
    let mut notes = load_one_shots()?;
    let terms = tokenize(topic);
    let empty = terms.is_empty();
    let mut candidates: Vec<(usize, i64, usize)> = notes
        .iter()
        .enumerate()
        .filter(|(_, note)| !note.seen)
        .map(|(i, note)| {
            let score = if empty {
                0
            } else {
                score_one_shot(note, &terms)
            };
            (score, note.created_at, i)
        })
        .filter(|(score, _, _)| empty || *score > 0)
        .collect();
    candidates.sort_by(|a, b| b.cmp(a));
    let selected: Vec<usize> = candidates.into_iter().take(n).map(|(_, _, i)| i).collect();
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    for &i in &selected {
        notes[i].seen = true;
    }
    let returned: Vec<OneShotNote> = selected.iter().map(|&i| notes[i].clone()).collect();
    notes.retain(|note| !note.seen);
    save_one_shots(&notes)?;
    Ok(returned)
}

#[allow(dead_code)]
pub fn recall_for_prompt(query: &str, limit: usize) -> Result<String> {
    recall_for_prompt_with_one_shot(query, limit, false)
}

pub fn recall_for_prompt_with_one_shot(
    query: &str,
    limit: usize,
    include_one_shot: bool,
) -> Result<String> {
    let notes = recall(query, limit)?;
    let shots = if include_one_shot {
        recall_one_shot(query, limit)?
    } else {
        Vec::new()
    };
    if notes.is_empty() && shots.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::from("\n\nRelevant memory (do not repeat work already noted):\n");
    for note in notes {
        let tags = if note.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", note.tags.join(", "))
        };
        out.push_str(&format!(
            "-{}{}\n",
            note.content.lines().next().unwrap_or(&note.content),
            tags
        ));
    }
    for shot in shots {
        out.push_str(&format!(
            "- [{}] {}\n",
            shot.topic,
            shot.detail.lines().next().unwrap_or(&shot.detail)
        ));
    }
    Ok(out)
}

pub fn compact() -> Result<usize> {
    let mut notes = load_notes()?;
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut duplicates: Vec<(usize, usize)> = Vec::new();
    for (i, n) in notes.iter().enumerate() {
        let key = n.content.trim().to_lowercase();
        if let Some(prev) = seen.get(&key).copied() {
            duplicates.push((prev, i));
        } else {
            seen.insert(key, i);
        }
    }
    for (keep, dup) in duplicates {
        let merged = {
            let mut t = notes[keep].tags.clone();
            t.extend(notes[dup].tags.clone());
            t.sort();
            t.dedup();
            t
        };
        notes[keep].tags = merged;
        notes[keep].created_at = notes[keep].created_at.max(notes[dup].created_at);
        notes[dup].content.clear();
    }
    let before = notes.len();
    notes.retain(|n| !n.content.is_empty());
    save_notes(&notes)?;
    Ok(before - notes.len())
}

pub fn run_memory(cmd: MemoryCommand) -> Result<()> {
    match cmd {
        MemoryCommand::Remember(args) => {
            let note = remember(&args.content, &args.tags)?;
            println!("remembered {} ({} tags)", note.id, note.tags.len());
        }
        MemoryCommand::Oneshot(args) => {
            let note = remember_one_shot(&args.topic, &args.detail)?;
            println!("recorded one-shot {} ({})", note.id, note.topic);
        }
        MemoryCommand::Recall(args) => {
            for note in recall(&args.query, args.limit)? {
                println!(
                    "{} {} [{}] {}",
                    note.id,
                    fmt_time(note.created_at),
                    note.tags.join(","),
                    note.content.lines().next().unwrap_or(&note.content)
                );
            }
        }
        MemoryCommand::List(args) => {
            for note in list(args.tag.as_deref(), args.limit)? {
                println!(
                    "{} {} [{}] {}",
                    note.id,
                    fmt_time(note.created_at),
                    note.tags.join(","),
                    note.content.lines().next().unwrap_or(&note.content)
                );
            }
        }
        MemoryCommand::Compact => {
            let removed = compact()?;
            println!("removed {removed} duplicate notes");
        }
    }
    Ok(())
}

fn fmt_time(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("omgb-memory-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn test_remember_and_recall() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        unsafe { std::env::set_var("OMGB_HOME", home.as_os_str()) };
        let note = remember("Use anyhow for error handling", &["rust".to_string()]).unwrap();
        let notes = recall("anyhow error", 5).unwrap();
        assert!(notes.iter().any(|n| n.id == note.id));
        std::fs::remove_dir_all(&home).ok();
        unsafe { std::env::remove_var("OMGB_HOME") };
    }

    #[test]
    fn test_list_and_compact() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        unsafe { std::env::set_var("OMGB_HOME", home.as_os_str()) };
        remember("duplicate", &[]).unwrap();
        remember("duplicate", &["tag".to_string()]).unwrap();
        let removed = compact().unwrap();
        assert_eq!(removed, 1);
        let notes = list(None, 10).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].tags.contains(&"tag".to_string()));
        std::fs::remove_dir_all(&home).ok();
        unsafe { std::env::remove_var("OMGB_HOME") };
    }

    #[test]
    fn test_remember_and_recall_one_shot() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        unsafe { std::env::set_var("OMGB_HOME", home.as_os_str()) };
        remember_one_shot("meeting", "discuss rust migration").unwrap();
        remember_one_shot("meeting", "discuss api keys").unwrap();
        let first = recall_one_shot("meeting", 1).unwrap();
        assert_eq!(first.len(), 1);
        assert!(first[0].seen);
        let second = recall_one_shot("meeting", 1).unwrap();
        assert_eq!(second.len(), 1);
        let third = recall_one_shot("meeting", 10).unwrap();
        assert!(third.is_empty());
        std::fs::remove_dir_all(&home).ok();
        unsafe { std::env::remove_var("OMGB_HOME") };
    }
}
