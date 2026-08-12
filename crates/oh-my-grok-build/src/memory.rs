//! Persistent cross-session memory store (JSONL-backed) for omgb.
//!
//! Notes are stored in `~/.omgb/memory.jsonl` with a small keyword search
//! index rebuilt on first access each run.  This is intentionally simple so
//! it works without embedding providers or a build dependency on sqlite-vec.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::args::{MemoryCommand, MemoryLeaseResolution};

const MEMORY_FILE: &str = "memory.jsonl";
const ONE_SHOT_FILE: &str = "one_shot_journal.jsonl";
const MAX_MEMORY_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MEMORY_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_MEMORY_QUERY_BYTES: usize = 64 * 1024;
const MAX_MEMORY_TAGS: usize = 64;
const MAX_MEMORY_TAG_BYTES: usize = 256;
const MAX_MEMORY_RESULTS: usize = 10_000;
const MAX_ONE_SHOT_TOPIC_BYTES: usize = 4096;

fn memory_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join(MEMORY_FILE))
}

fn one_shot_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join(ONE_SHOT_FILE))
}

fn lock_for(path: &Path) -> PathBuf {
    path.with_extension("jsonl.lock")
}

fn with_file_lock<R>(path: &Path, exclusive: bool, f: impl FnOnce() -> Result<R>) -> Result<R> {
    let lock = lock_for(path);
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Ok(metadata) = std::fs::symlink_metadata(&lock)
        && !metadata.file_type().is_file()
    {
        bail!("memory lock is not a regular file: {}", lock.display());
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock)?;
    if !file.metadata()?.file_type().is_file() {
        bail!("memory lock is not a regular file: {}", lock.display());
    }
    if exclusive {
        file.lock_exclusive()?;
    } else {
        file.lock_shared()?;
    }
    let result = f();
    let _ = file.unlock();
    result
}

fn ensure_store() -> Result<()> {
    let dir = crate::providers::omg_dir()?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }
    let path = dir.join(MEMORY_FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                Ok(_) => crate::providers::restrict_omg_file_permissions(&path)?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error).context("create memory store"),
            }
        }
        Err(error) => return Err(error).context("inspect memory store"),
    }
    ensure_regular_file(&path)?;
    Ok(())
}

fn ensure_regular_file(path: &Path) -> Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect memory store {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!("memory store is not a regular file: {}", path.display());
    }
    Ok(metadata)
}

fn load_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                bail!("memory store is not a regular file: {}", path.display());
            }
            metadata
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect memory store {}", path.display()));
        }
    };
    if metadata.len() > MAX_MEMORY_FILE_BYTES {
        bail!(
            "memory store {} exceeds the {MAX_MEMORY_FILE_BYTES} byte safety limit",
            path.display()
        );
    }
    if metadata.len() == 0 {
        return Ok(Vec::new());
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("read memory store {}", path.display()))?;
    contents
        .lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).with_context(|| {
                format!(
                    "invalid JSON record in memory store {} at line {}",
                    path.display(),
                    index + 1
                )
            })
        })
        .collect()
}

fn validate_text(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} must not be empty");
    }
    if value.len() > max_bytes {
        bail!("{label} is too large (max {max_bytes} bytes)");
    }
    if value.contains('\0') {
        bail!("{label} must not contain NUL characters");
    }
    Ok(())
}

fn validate_limit(limit: usize) -> Result<()> {
    if limit > MAX_MEMORY_RESULTS {
        bail!("memory result limit is too large (max {MAX_MEMORY_RESULTS})");
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_owner_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_owner_start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_prompt_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_yolo: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_phase: Option<String>,
}

pub struct OneShotLeaseGuard {
    lease_id: String,
    notes: Vec<OneShotNote>,
    resolved: bool,
    reported: bool,
}

impl OneShotLeaseGuard {
    pub fn notes(&self) -> &[OneShotNote] {
        &self.notes
    }

    pub fn bind_attempt(
        &mut self,
        session_id: Option<&str>,
        model: &str,
        yolo: bool,
    ) -> Result<()> {
        bind_one_shot_lease_attempt(&self.lease_id, session_id, model, yolo)?;
        for note in &mut self.notes {
            note.lease_session_id = session_id.map(str::to_string);
            note.lease_model = Some(model.to_string());
            note.lease_yolo = Some(yolo);
            note.lease_phase = Some("running".into());
        }
        Ok(())
    }

    pub fn consume(mut self) -> Result<()> {
        match finish_one_shot_lease(&self.lease_id, true) {
            Ok(()) => {
                self.resolved = true;
                Ok(())
            }
            Err(error) => {
                eprintln!(
                    "warning: one-shot memory lease {} remains ambiguous after successful execution: {error}; inspect `omgb memory leases` and resolve it explicitly",
                    self.lease_id
                );
                self.reported = true;
                Err(error)
            }
        }
    }
}

impl Drop for OneShotLeaseGuard {
    fn drop(&mut self) {
        if !self.resolved && !self.reported {
            eprintln!(
                "warning: one-shot memory lease {} remains ambiguous; inspect `omgb memory leases` and use `omgb memory resolve-oneshot {} consume|retry --confirm` after reconciling the recorded owner/session",
                self.lease_id, self.lease_id
            );
        }
    }
}

fn load_notes() -> Result<Vec<MemoryNote>> {
    ensure_store()?;
    let path = memory_path()?;
    load_jsonl(&path)
}

fn save_notes(notes: &[MemoryNote]) -> Result<()> {
    ensure_store()?;
    let path = memory_path()?;
    let mut content = String::new();
    for note in notes {
        content.push_str(&serde_json::to_string(note)?);
        content.push('\n');
    }
    if content.len() as u64 > MAX_MEMORY_FILE_BYTES {
        bail!("memory store exceeds the {MAX_MEMORY_FILE_BYTES} byte safety limit");
    }
    crate::providers::write_file_atomic(&path, content, true)
}

fn load_one_shots() -> Result<Vec<OneShotNote>> {
    let path = one_shot_path()?;
    load_jsonl(&path)
}

fn save_one_shots(notes: &[OneShotNote]) -> Result<()> {
    let path = one_shot_path()?;
    let mut content = String::new();
    for note in notes {
        content.push_str(&serde_json::to_string(note)?);
        content.push('\n');
    }
    if content.len() as u64 > MAX_MEMORY_FILE_BYTES {
        bail!("one-shot store exceeds the {MAX_MEMORY_FILE_BYTES} byte safety limit");
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
    validate_text("memory content", content, MAX_MEMORY_CONTENT_BYTES)?;
    if tags.len() > MAX_MEMORY_TAGS {
        bail!("too many memory tags (max {MAX_MEMORY_TAGS})");
    }
    for tag in tags {
        validate_text("memory tag", tag, MAX_MEMORY_TAG_BYTES)?;
        if tag.chars().any(char::is_control) {
            bail!("memory tags must not contain control characters");
        }
    }
    let path = memory_path()?;
    with_file_lock(&path, true, || {
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
    })
}

pub fn remember_one_shot(topic: &str, detail: &str) -> Result<OneShotNote> {
    validate_text("one-shot topic", topic, MAX_ONE_SHOT_TOPIC_BYTES)?;
    validate_text("one-shot detail", detail, MAX_MEMORY_CONTENT_BYTES)?;
    if topic.chars().any(char::is_control) {
        bail!("one-shot topic must not contain control characters");
    }
    let path = one_shot_path()?;
    with_file_lock(&path, true, || {
        let mut notes = load_one_shots()?;
        let topic_norm = topic.trim().to_lowercase();
        let detail_norm = detail.trim().to_lowercase();
        if let Some(existing) = notes.iter().find(|n| {
            n.topic.trim().to_lowercase() == topic_norm
                && n.detail.trim().to_lowercase() == detail_norm
        }) {
            return Ok(existing.clone());
        }
        let note = OneShotNote {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: now(),
            topic: topic.to_string(),
            detail: detail.to_string(),
            seen: false,
            lease_id: None,
            lease_expires_at: None,
            lease_started_at: None,
            lease_owner_pid: None,
            lease_owner_start: None,
            lease_prompt_hash: None,
            lease_session_id: None,
            lease_model: None,
            lease_yolo: None,
            lease_phase: None,
        };
        notes.push(note.clone());
        save_one_shots(&notes)?;
        Ok(note)
    })
}

pub fn list(tag: Option<&str>, limit: usize) -> Result<Vec<MemoryNote>> {
    validate_limit(limit)?;
    if let Some(tag) = tag {
        validate_text("memory tag", tag, MAX_MEMORY_TAG_BYTES)?;
        if tag.chars().any(char::is_control) {
            bail!("memory tag must not contain control characters");
        }
    }
    let path = memory_path()?;
    with_file_lock(&path, false, || {
        let mut notes = load_notes()?;
        notes.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(notes
            .into_iter()
            .filter(|n| tag.is_none_or(|t| n.tags.iter().any(|x| x == t)))
            .take(limit)
            .collect())
    })
}

pub fn recall(query: &str, limit: usize) -> Result<Vec<MemoryNote>> {
    validate_limit(limit)?;
    if query.len() > MAX_MEMORY_QUERY_BYTES {
        bail!("memory query is too large (max {MAX_MEMORY_QUERY_BYTES} bytes)");
    }
    if query.contains('\0') {
        bail!("memory query must not contain NUL characters");
    }
    let terms = tokenize(query);
    if terms.is_empty() {
        return list(None, limit);
    }
    let path = memory_path()?;
    with_file_lock(&path, false, || {
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
    })
}

pub fn recall_one_shot(topic: &str, n: usize) -> Result<Vec<OneShotNote>> {
    validate_limit(n)?;
    if topic.len() > MAX_MEMORY_QUERY_BYTES {
        bail!("one-shot query is too large (max {MAX_MEMORY_QUERY_BYTES} bytes)");
    }
    if topic.contains('\0') {
        bail!("one-shot query must not contain NUL characters");
    }
    let path = one_shot_path()?;
    with_file_lock(&path, true, || {
        let mut notes = load_one_shots()?;
        let terms = tokenize(topic);
        let empty = terms.is_empty();
        let mut candidates: Vec<(usize, i64, usize)> = notes
            .iter()
            .enumerate()
            .filter(|(_, note)| !note.seen && note.lease_id.is_none())
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
    })
}

pub fn lease_one_shot(topic: &str, n: usize) -> Result<Option<OneShotLeaseGuard>> {
    validate_limit(n)?;
    if topic.len() > MAX_MEMORY_QUERY_BYTES {
        bail!("one-shot query is too large (max {MAX_MEMORY_QUERY_BYTES} bytes)");
    }
    let owner_pid = std::process::id();
    let owner_start = crate::lsp::process_start_identity(owner_pid)
        .context("bind one-shot lease to the current process")?;
    let lease_started_at = now();
    let prompt_hash = blake3::hash(topic.as_bytes()).to_hex().to_string();
    let path = one_shot_path()?;
    with_file_lock(&path, true, || {
        let mut notes = load_one_shots()?;
        let terms = tokenize(topic);
        let empty = terms.is_empty();
        let mut candidates: Vec<(usize, i64, usize)> = notes
            .iter()
            .enumerate()
            .filter(|(_, note)| !note.seen && note.lease_id.is_none())
            .map(|(index, note)| {
                let score = if empty {
                    0
                } else {
                    score_one_shot(note, &terms)
                };
                (score, note.created_at, index)
            })
            .filter(|(score, _, _)| empty || *score > 0)
            .collect();
        candidates.sort_by(|a, b| b.cmp(a));
        let selected: Vec<usize> = candidates
            .into_iter()
            .take(n)
            .map(|(_, _, index)| index)
            .collect();
        if selected.is_empty() {
            return Ok(None);
        }
        let lease_id = uuid::Uuid::new_v4().to_string();
        for &index in &selected {
            notes[index].lease_id = Some(lease_id.clone());
            notes[index].lease_expires_at = None;
            notes[index].lease_started_at = Some(lease_started_at);
            notes[index].lease_owner_pid = Some(owner_pid);
            notes[index].lease_owner_start = Some(owner_start);
            notes[index].lease_prompt_hash = Some(prompt_hash.clone());
            notes[index].lease_session_id = None;
            notes[index].lease_model = None;
            notes[index].lease_yolo = None;
            notes[index].lease_phase = Some("leased".into());
        }
        let leased = selected.iter().map(|&index| notes[index].clone()).collect();
        save_one_shots(&notes)?;
        Ok(Some(OneShotLeaseGuard {
            lease_id,
            notes: leased,
            resolved: false,
            reported: false,
        }))
    })
}

fn bind_one_shot_lease_attempt(
    lease_id: &str,
    session_id: Option<&str>,
    model: &str,
    yolo: bool,
) -> Result<()> {
    if let Some(session_id) = session_id {
        crate::threads::validate_id(session_id)?;
    }
    if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        bail!("one-shot lease model identity is invalid");
    }
    let path = one_shot_path()?;
    with_file_lock(&path, true, || {
        let mut notes = load_one_shots()?;
        let mut found = false;
        for note in &mut notes {
            if note.lease_id.as_deref() == Some(lease_id) {
                found = true;
                note.lease_session_id = session_id.map(str::to_string);
                note.lease_model = Some(model.to_string());
                note.lease_yolo = Some(yolo);
                note.lease_phase = Some("running".into());
            }
        }
        if !found {
            bail!("one-shot lease '{lease_id}' was not found");
        }
        save_one_shots(&notes)
    })
}

fn one_shot_lease_owner_is_live(note: &OneShotNote) -> Result<bool> {
    let pid = note
        .lease_owner_pid
        .context("one-shot lease has no owner PID; consume or migrate it before retrying")?;
    let expected_start = note.lease_owner_start.context(
        "one-shot lease has no process-start identity; consume or migrate it before retrying",
    )?;
    if !crate::process_alive(pid) {
        return Ok(false);
    }
    Ok(crate::lsp::process_start_identity(pid)? == expected_start)
}

fn finish_one_shot_lease(lease_id: &str, consume: bool) -> Result<()> {
    let path = one_shot_path()?;
    with_file_lock(&path, true, || {
        let mut notes = load_one_shots()?;
        if !notes
            .iter()
            .any(|note| note.lease_id.as_deref() == Some(lease_id))
        {
            bail!("one-shot lease '{lease_id}' was not found");
        }
        if !consume {
            for note in notes
                .iter()
                .filter(|note| note.lease_id.as_deref() == Some(lease_id))
            {
                if one_shot_lease_owner_is_live(note)? {
                    bail!(
                        "one-shot lease '{lease_id}' is still owned by live pid {}; refusing concurrent retry",
                        note.lease_owner_pid.unwrap_or_default()
                    );
                }
            }
        }
        if consume {
            notes.retain(|note| note.lease_id.as_deref() != Some(lease_id));
        } else {
            for note in &mut notes {
                if note.lease_id.as_deref() == Some(lease_id) {
                    note.lease_id = None;
                    note.lease_expires_at = None;
                    note.lease_started_at = None;
                    note.lease_owner_pid = None;
                    note.lease_owner_start = None;
                    note.lease_prompt_hash = None;
                    note.lease_session_id = None;
                    note.lease_model = None;
                    note.lease_yolo = None;
                    note.lease_phase = None;
                }
            }
        }
        save_one_shots(&notes)
    })
}

fn list_one_shot_leases() -> Result<Vec<OneShotNote>> {
    let path = one_shot_path()?;
    with_file_lock(&path, false, || {
        Ok(load_one_shots()?
            .into_iter()
            .filter(|note| note.lease_id.is_some())
            .collect())
    })
}

pub fn format_prompt_memory(notes: Vec<MemoryNote>, shots: &[OneShotNote]) -> String {
    if notes.is_empty() && shots.is_empty() {
        return String::new();
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
    out
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
    Ok(format_prompt_memory(notes, &shots))
}

pub fn compact() -> Result<usize> {
    let path = memory_path()?;
    with_file_lock(&path, true, || {
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
    })
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
        MemoryCommand::Leases => {
            let notes = list_one_shot_leases()?;
            if notes.is_empty() {
                println!("no ambiguous one-shot deliveries");
            }
            for note in notes {
                println!(
                    "{} lease={} started={} owner={}:{} phase={} session={} model={} yolo={} [{}] {}",
                    note.id,
                    note.lease_id.as_deref().unwrap_or("-"),
                    note.lease_started_at
                        .map(fmt_time)
                        .unwrap_or_else(|| "legacy".into()),
                    note.lease_owner_pid
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "-".into()),
                    note.lease_owner_start
                        .map(|start| start.to_string())
                        .unwrap_or_else(|| "-".into()),
                    note.lease_phase.as_deref().unwrap_or("unknown"),
                    note.lease_session_id.as_deref().unwrap_or("-"),
                    note.lease_model.as_deref().unwrap_or("-"),
                    note.lease_yolo
                        .map(|yolo| yolo.to_string())
                        .unwrap_or_else(|| "-".into()),
                    note.topic,
                    note.detail.lines().next().unwrap_or(&note.detail)
                );
            }
        }
        MemoryCommand::ResolveOneshot {
            lease,
            action,
            confirm,
        } => {
            uuid::Uuid::parse_str(&lease).context("one-shot lease must be a UUID")?;
            if !confirm {
                bail!("refusing to resolve an ambiguous one-shot lease without --confirm");
            }
            finish_one_shot_lease(&lease, matches!(action, MemoryLeaseResolution::Consume))?;
            println!(
                "resolved one-shot lease {lease}: {}",
                match action {
                    MemoryLeaseResolution::Consume => "consumed",
                    MemoryLeaseResolution::Retry => "returned to pending",
                }
            );
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
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let note = remember("Use anyhow for error handling", &["rust".to_string()]).unwrap();
        let notes = recall("anyhow error", 5).unwrap();
        assert!(notes.iter().any(|n| n.id == note.id));
        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn test_list_and_compact() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        remember("duplicate", &[]).unwrap();
        remember("duplicate", &["tag".to_string()]).unwrap();
        let removed = compact().unwrap();
        assert_eq!(removed, 1);
        let notes = list(None, 10).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].tags.contains(&"tag".to_string()));
        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn test_remember_and_recall_one_shot() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
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
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn one_shot_lease_requires_explicit_resolution_after_interruption() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let note = remember_one_shot("deploy", "verify runtime first").unwrap();
        {
            let mut lease = lease_one_shot("deploy", 1).unwrap().unwrap();
            assert_eq!(lease.notes()[0].id, note.id);
            lease
                .bind_attempt(Some("session-1"), "omgb-test", true)
                .unwrap();
            assert!(lease_one_shot("deploy", 1).unwrap().is_none());
        }
        assert!(lease_one_shot("deploy", 1).unwrap().is_none());
        let leased_note = list_one_shot_leases().unwrap().remove(0);
        assert_eq!(leased_note.lease_session_id.as_deref(), Some("session-1"));
        assert_eq!(leased_note.lease_model.as_deref(), Some("omgb-test"));
        assert_eq!(leased_note.lease_yolo, Some(true));
        assert_eq!(leased_note.lease_phase.as_deref(), Some("running"));
        let lease_id = leased_note.lease_id.unwrap();
        assert!(finish_one_shot_lease(&lease_id, false).is_err());
        let path = one_shot_path().unwrap();
        with_file_lock(&path, true, || {
            let mut notes = load_one_shots()?;
            for note in &mut notes {
                if note.lease_id.as_deref() == Some(lease_id.as_str()) {
                    note.lease_owner_pid = Some(u32::MAX);
                    note.lease_owner_start = Some(0);
                }
            }
            save_one_shots(&notes)
        })
        .unwrap();
        finish_one_shot_lease(&lease_id, false).unwrap();
        let lease = lease_one_shot("deploy", 1).unwrap().unwrap();
        lease.consume().unwrap();
        assert!(lease_one_shot("deploy", 1).unwrap().is_none());
        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn test_remember_one_shot_dedup() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let a = remember_one_shot("todo", "fix permissions").unwrap();
        let b = remember_one_shot("todo", "fix permissions").unwrap();
        assert_eq!(a.id, b.id);
        let all = recall_one_shot("todo", 10).unwrap();
        assert_eq!(all.len(), 1);
        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn malformed_jsonl_fails_closed_without_rewriting_store() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        std::fs::create_dir_all(&home).unwrap();
        let path = home.join(MEMORY_FILE);
        let valid = serde_json::to_string(&MemoryNote {
            id: "valid".into(),
            created_at: 1,
            tags: vec![],
            content: "keep me".into(),
            access_count: 0,
        })
        .unwrap();
        let original = format!("{valid}\n{{not-json}}\n");
        std::fs::write(&path, &original).unwrap();

        let error = compact().unwrap_err().to_string();
        assert!(error.contains("line 2"), "unexpected error: {error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn memory_inputs_and_result_limits_are_bounded() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));

        assert!(remember("   ", &[]).is_err());
        assert!(remember(&"x".repeat(MAX_MEMORY_CONTENT_BYTES + 1), &[]).is_err());
        assert!(remember("valid", &vec!["tag".into(); MAX_MEMORY_TAGS + 1]).is_err());
        assert!(remember_one_shot("topic\nname", "detail").is_err());
        assert!(list(None, MAX_MEMORY_RESULTS + 1).is_err());
        assert!(recall(&"q".repeat(MAX_MEMORY_QUERY_BYTES + 1), 1).is_err());

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }
}
