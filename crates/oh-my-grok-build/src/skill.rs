//! Auto skill creation and retrieval for `omgb`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::prompt_guard;
use crate::timeline::TimelineEvent;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Skill {
    pub name: String,
    pub trigger: String,
    pub steps: Vec<String>,
    pub pitfalls: Vec<String>,
    pub verification: Vec<String>,
    #[serde(skip)]
    pub path: PathBuf,
}

const MAX_SKILL_BYTES: usize = 64 * 1024;
const MAX_PROPOSAL_BYTES: usize = 256 * 1024;
const MAX_PROPOSALS: usize = 4096;
const MAX_SKILL_ITEMS: usize = 64;
const MAX_SKILL_ITEM_BYTES: usize = 2048;
const MAX_REFINEMENT_SOURCE_BYTES: usize = 256 * 1024;
const MAX_REFINEMENT_NOTE_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefinementStatus {
    Proposed,
    Applying,
    Active,
    Rejected,
    RollingBack,
    RolledBack,
    Ambiguous,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinementProposal {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub status: RefinementStatus,
    pub source_sha256: String,
    pub candidate_sha256: String,
    pub candidate: Skill,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_skill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn skills_dir() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("skills"))
}

fn refinements_dir() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("refinements"))
}

fn refinement_lock() -> Result<std::fs::File> {
    let dir = crate::providers::omg_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("refinements.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    crate::providers::restrict_omg_file_permissions(&path)?;
    file.lock_exclusive()?;
    Ok(file)
}

fn refinement_path(id: &str) -> Result<PathBuf> {
    crate::threads::validate_id(id)?;
    Ok(refinements_dir()?.join(format!("{id}.json")))
}

fn skill_path(name: &str) -> Result<PathBuf> {
    Ok(skills_dir()?.join(format!("{}.md", safe_filename(name))))
}

fn sha256(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn validate_skill(skill: &Skill) -> Result<()> {
    if skill.name.trim().is_empty() || skill.name.len() > 120 {
        bail!("skill name must be non-empty and at most 120 bytes");
    }
    if skill.trigger.trim().is_empty() || skill.trigger.len() > 512 {
        bail!("generated skills require a non-empty trigger of at most 512 bytes");
    }
    for (label, items) in [
        ("steps", &skill.steps),
        ("pitfalls", &skill.pitfalls),
        ("verification", &skill.verification),
    ] {
        if items.len() > MAX_SKILL_ITEMS {
            bail!("skill {label} exceeds the {MAX_SKILL_ITEMS} item limit");
        }
        if items.iter().any(|item| {
            item.trim().is_empty() || item.len() > MAX_SKILL_ITEM_BYTES || item.contains('\0')
        }) {
            bail!(
                "skill {label} entries must be non-empty, contain no NUL, and be at most {MAX_SKILL_ITEM_BYTES} bytes"
            );
        }
    }
    if skill.steps.is_empty() || skill.verification.is_empty() {
        bail!("generated skills require at least one step and one verification gate");
    }
    let bytes = format_skill_markdown(skill)?;
    if bytes.len() > MAX_SKILL_BYTES {
        bail!("skill exceeds the {MAX_SKILL_BYTES} byte limit");
    }
    Ok(())
}

fn safe_filename(name: &str) -> String {
    let mut out = String::new();
    let mut prev = '_';
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
            prev = c;
        } else if prev != '_' {
            out.push('_');
            prev = '_';
        }
    }
    let out = out.trim_end_matches('_').to_lowercase();
    if out.is_empty() || out == "_" {
        "untitled".into()
    } else {
        out
    }
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    if !text.starts_with("+++\n") {
        return None;
    }
    let rest = &text[4..];
    let end = rest.find("\n+++")?;
    let front = &rest[..end];
    let after = &rest[end + 4..];
    Some((front, after.strip_prefix('\n').unwrap_or(after)))
}

fn format_list(items: &[String]) -> String {
    if items.is_empty() {
        "- none\n".into()
    } else {
        items.iter().map(|s| format!("- {s}\n")).collect()
    }
}

pub fn format_skill_markdown(skill: &Skill) -> Result<String> {
    let front = toml::to_string(skill)?;
    let body = format!(
        "# {}\n\n> Trigger: `{}`\n\n## Steps\n{}\n## Pitfalls\n{}\n## Verification\n{}\n",
        skill.name,
        skill.trigger,
        format_list(&skill.steps),
        format_list(&skill.pitfalls),
        format_list(&skill.verification)
    );
    Ok(format!("+++\n{front}+++\n\n{body}"))
}

fn parse_skill_markdown(text: &str) -> Result<Skill> {
    let (front, _body) = split_frontmatter(text)
        .ok_or_else(|| anyhow::anyhow!("skill markdown has no TOML frontmatter"))?;
    toml::from_str(front).map_err(|e| anyhow::anyhow!("invalid skill frontmatter: {e}"))
}

/// Write a skill to `~/.omgb/skills/{name}.md`.
pub fn write_skill(skill: &Skill) -> Result<()> {
    validate_skill(skill)?;
    let dir = skills_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = skill_path(&skill.name)?;
    crate::providers::write_file_atomic(&path, format_skill_markdown(skill)?, true)
}

fn read_skill_text(path: &Path) -> Result<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            metadata
        }
        Ok(_) => bail!(
            "active skill path is not a regular file: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_SKILL_BYTES as u64 {
        bail!("active skill exceeds the {MAX_SKILL_BYTES} byte limit");
    }
    Ok(Some(std::fs::read_to_string(path)?))
}

fn save_proposal(proposal: &RefinementProposal) -> Result<()> {
    let path = refinement_path(&proposal.id)?;
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("refinement path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec_pretty(proposal)?;
    if bytes.len() > MAX_PROPOSAL_BYTES {
        bail!("refinement proposal exceeds the {MAX_PROPOSAL_BYTES} byte limit");
    }
    crate::providers::write_file_atomic(&path, bytes, true)
}

fn load_proposal_unlocked(id: &str) -> Result<RefinementProposal> {
    let path = refinement_path(id)?;
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!(
            "refinement proposal is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() > MAX_PROPOSAL_BYTES as u64 {
        bail!("refinement proposal exceeds the {MAX_PROPOSAL_BYTES} byte limit");
    }
    let proposal: RefinementProposal = serde_json::from_slice(&std::fs::read(&path)?)?;
    if proposal.id != id {
        bail!("refinement proposal identity does not match its file name");
    }
    validate_skill(&proposal.candidate)?;
    let candidate = format_skill_markdown(&proposal.candidate)?;
    if sha256(candidate.as_bytes()) != proposal.candidate_sha256 {
        bail!("refinement proposal candidate hash does not match its content");
    }
    Ok(proposal)
}

fn expected_prior_matches(proposal: &RefinementProposal, current: Option<&str>) -> bool {
    match (proposal.previous_skill.as_deref(), current) {
        (None, None) => true,
        (Some(expected), Some(actual)) => sha256(expected.as_bytes()) == sha256(actual.as_bytes()),
        _ => false,
    }
}

fn reconcile_proposal(proposal: &mut RefinementProposal) -> Result<bool> {
    if !matches!(
        proposal.status,
        RefinementStatus::Applying | RefinementStatus::RollingBack
    ) {
        return Ok(false);
    }
    let path = skill_path(&proposal.candidate.name)?;
    let current = read_skill_text(&path)?;
    let candidate_active = current
        .as_deref()
        .is_some_and(|text| sha256(text.as_bytes()) == proposal.candidate_sha256);
    proposal.status = match proposal.status {
        RefinementStatus::Applying if candidate_active => RefinementStatus::Active,
        RefinementStatus::Applying if expected_prior_matches(proposal, current.as_deref()) => {
            RefinementStatus::Proposed
        }
        RefinementStatus::RollingBack if expected_prior_matches(proposal, current.as_deref()) => {
            RefinementStatus::RolledBack
        }
        RefinementStatus::RollingBack if candidate_active => RefinementStatus::Active,
        _ => RefinementStatus::Ambiguous,
    };
    proposal.updated_at = Utc::now();
    proposal.note = Some(match proposal.status {
        RefinementStatus::Active => "reconciled active candidate after an interrupted write".into(),
        RefinementStatus::Proposed => {
            "reconciled unchanged prior state after an interrupted approval".into()
        }
        RefinementStatus::RolledBack => "reconciled completed rollback".into(),
        RefinementStatus::Ambiguous => {
            "active skill drifted during an interrupted refinement; manual review required".into()
        }
        _ => unreachable!(),
    });
    save_proposal(proposal)?;
    Ok(true)
}

pub fn propose_skill(skill: Skill, source: &str) -> Result<RefinementProposal> {
    validate_skill(&skill)?;
    let _lock = refinement_lock()?;
    let dir = refinements_dir()?;
    std::fs::create_dir_all(&dir)?;
    if std::fs::read_dir(&dir)?.take(MAX_PROPOSALS + 1).count() >= MAX_PROPOSALS {
        bail!("refinement proposal store reached its {MAX_PROPOSALS} record limit");
    }
    let candidate = format_skill_markdown(&skill)?;
    let now = Utc::now();
    let proposal = RefinementProposal {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: now,
        updated_at: now,
        status: RefinementStatus::Proposed,
        source_sha256: sha256(source.as_bytes()),
        candidate_sha256: sha256(candidate.as_bytes()),
        candidate: skill,
        previous_skill: None,
        note: Some(
            "generated from a qualifying timeline trajectory; not active until approved".into(),
        ),
    };
    save_proposal(&proposal)?;
    Ok(proposal)
}

pub fn list_proposals() -> Result<Vec<RefinementProposal>> {
    let _lock = refinement_lock()?;
    let dir = refinements_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut proposals = Vec::new();
    for entry in std::fs::read_dir(&dir)?.take(MAX_PROPOSALS + 1) {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let mut proposal = load_proposal_unlocked(id)?;
            reconcile_proposal(&mut proposal)?;
            proposals.push(proposal);
        }
    }
    if proposals.len() > MAX_PROPOSALS {
        bail!("refinement proposal store exceeds its {MAX_PROPOSALS} record limit");
    }
    proposals.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(proposals)
}

pub fn load_proposal(id: &str) -> Result<RefinementProposal> {
    let _lock = refinement_lock()?;
    let mut proposal = load_proposal_unlocked(id)?;
    reconcile_proposal(&mut proposal)?;
    Ok(proposal)
}

pub fn approve_proposal(id: &str, confirm: bool) -> Result<RefinementProposal> {
    if !confirm {
        bail!("refusing to activate a harness refinement without --confirm");
    }
    let _lock = refinement_lock()?;
    let mut proposal = load_proposal_unlocked(id)?;
    reconcile_proposal(&mut proposal)?;
    if proposal.status != RefinementStatus::Proposed {
        bail!("refinement proposal is not awaiting approval");
    }
    validate_skill(&proposal.candidate)?;
    let path = skill_path(&proposal.candidate.name)?;
    proposal.previous_skill = read_skill_text(&path)?;
    proposal.status = RefinementStatus::Applying;
    proposal.updated_at = Utc::now();
    proposal.note = Some("approval recorded; candidate publication in progress".into());
    save_proposal(&proposal)?;
    let before_publish = read_skill_text(&path)?;
    if !expected_prior_matches(&proposal, before_publish.as_deref()) {
        proposal.status = RefinementStatus::Ambiguous;
        proposal.updated_at = Utc::now();
        proposal.note = Some(
            "active skill changed during approval; publication was refused to avoid clobbering it"
                .into(),
        );
        save_proposal(&proposal)?;
        bail!("active skill changed during approval; publication requires manual reconciliation");
    }
    write_skill(&proposal.candidate)?;
    proposal.status = RefinementStatus::Active;
    proposal.updated_at = Utc::now();
    proposal.note =
        Some("candidate passed deterministic validation and was approved by an operator".into());
    save_proposal(&proposal)?;
    Ok(proposal)
}

pub fn reject_proposal(id: &str, reason: Option<String>) -> Result<RefinementProposal> {
    if reason
        .as_ref()
        .is_some_and(|value| value.len() > MAX_REFINEMENT_NOTE_BYTES || value.contains('\0'))
    {
        bail!(
            "refinement rejection reason must contain no NUL and be at most {MAX_REFINEMENT_NOTE_BYTES} bytes"
        );
    }
    let _lock = refinement_lock()?;
    let mut proposal = load_proposal_unlocked(id)?;
    reconcile_proposal(&mut proposal)?;
    if proposal.status != RefinementStatus::Proposed {
        bail!("only a proposed refinement can be rejected");
    }
    proposal.status = RefinementStatus::Rejected;
    proposal.updated_at = Utc::now();
    proposal.note = Some(reason.unwrap_or_else(|| "rejected by operator".into()));
    save_proposal(&proposal)?;
    Ok(proposal)
}

pub fn rollback_proposal(id: &str, confirm: bool) -> Result<RefinementProposal> {
    if !confirm {
        bail!("refusing to roll back active harness context without --confirm");
    }
    let _lock = refinement_lock()?;
    let mut proposal = load_proposal_unlocked(id)?;
    reconcile_proposal(&mut proposal)?;
    if proposal.status != RefinementStatus::Active {
        bail!("only an active refinement can be rolled back");
    }
    let path = skill_path(&proposal.candidate.name)?;
    let current = read_skill_text(&path)?;
    if current
        .as_deref()
        .is_none_or(|text| sha256(text.as_bytes()) != proposal.candidate_sha256)
    {
        proposal.status = RefinementStatus::Ambiguous;
        proposal.updated_at = Utc::now();
        proposal.note = Some(
            "active skill changed after promotion; rollback refused to avoid clobbering it".into(),
        );
        save_proposal(&proposal)?;
        bail!("active skill drifted after approval; rollback requires manual reconciliation");
    }
    proposal.status = RefinementStatus::RollingBack;
    proposal.updated_at = Utc::now();
    proposal.note = Some("rollback recorded; prior state restoration in progress".into());
    save_proposal(&proposal)?;
    match proposal.previous_skill.as_deref() {
        Some(previous) => crate::providers::write_file_atomic(&path, previous, true)?,
        None => std::fs::remove_file(&path)?,
    }
    proposal.status = RefinementStatus::RolledBack;
    proposal.updated_at = Utc::now();
    proposal.note = Some("operator restored the recorded prior harness state".into());
    save_proposal(&proposal)?;
    Ok(proposal)
}

fn plugin_skills_dirs() -> Vec<PathBuf> {
    let plugins_dir = crate::providers::omg_dir().ok().map(|d| d.join("plugins"));
    let Some(plugins_dir) = plugins_dir else {
        return Vec::new();
    };
    if !plugins_dir.is_dir() {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(&plugins_dir).into_iter().flatten() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() && !ft.is_symlink() {
            let skills = path.join("skills");
            if let Ok(meta) = std::fs::symlink_metadata(&skills)
                && meta.is_dir()
                && !meta.file_type().is_symlink()
            {
                dirs.push(skills);
            }
        }
    }
    dirs
}

fn collect_skills_in_dir(dir: &Path, skills: &mut Vec<Skill>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(meta) = entry.file_type() else {
            continue;
        };
        if !meta.is_file() || meta.is_symlink() {
            continue;
        }
        if path.extension().is_some_and(|e| e == "md") {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            match parse_skill_markdown(&text) {
                Ok(mut skill) => {
                    skill.path = path;
                    skills.push(skill);
                }
                Err(e) => eprintln!("warning: skipping invalid skill {}: {e}", path.display()),
            }
        }
    }
}

/// List all persisted skills, including skills hot-loaded from installed plugins.
pub fn list_skills() -> Result<Vec<Skill>> {
    let mut skills = Vec::new();
    if let Ok(dir) = skills_dir() {
        collect_skills_in_dir(&dir, &mut skills);
    }
    for dir in plugin_skills_dirs() {
        collect_skills_in_dir(&dir, &mut skills);
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

fn path_components(path: &str) -> Vec<String> {
    path.split(['/', '\\'])
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn trigger_matches(cwd: &str, trigger: &str) -> bool {
    if trigger.is_empty() {
        return true;
    }
    let cwd = path_components(cwd);
    let trigger = path_components(trigger);
    if trigger.len() == 1 {
        cwd.iter().any(|c| c == &trigger[0])
    } else {
        cwd.windows(trigger.len()).any(|w| w == trigger.as_slice())
    }
}

/// Return a markdown string of skills whose trigger matches the current directory.
pub fn skill_preamble() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let skills = match list_skills() {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let mut relevant = Vec::new();
    for s in skills
        .into_iter()
        .filter(|s| trigger_matches(&cwd, &s.trigger))
    {
        let Some(name) = prompt_guard::sanitize_inline(&s.name, 120) else {
            continue;
        };
        let body = if s.path.exists() {
            std::fs::read_to_string(&s.path)
                .ok()
                .and_then(|text| split_frontmatter(&text).map(|(_, b)| b.to_string()))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let Some(body) = prompt_guard::sanitize_skill_body(&body, 2000) else {
            continue;
        };
        if !body.is_empty() {
            relevant.push(format!("# {name}\n\n{body}"));
        }
    }
    if relevant.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", relevant.join("\n\n---\n\n"))
    }
}

fn group_runs(events: Vec<TimelineEvent>) -> Vec<Vec<TimelineEvent>> {
    let mut runs: Vec<Vec<TimelineEvent>> = Vec::new();
    for ev in events {
        if ev.category == "exec" || ev.category == "run" || ev.category == "autonomous" {
            runs.push(vec![ev]);
        } else if let Some(last) = runs.last_mut() {
            last.push(ev);
        } else {
            runs.push(vec![ev]);
        }
    }
    runs
}

fn tool_call_count(e: &TimelineEvent) -> usize {
    if e.category == "tool" || e.category == "tool_call" {
        e.data
            .as_ref()
            .and_then(|d| d.get("tool_calls"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize
    } else {
        e.data
            .as_ref()
            .and_then(|d| d.get("tool_calls"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize
    }
}

fn run_has_errors(run: &[TimelineEvent]) -> bool {
    run.iter().any(|e| {
        e.category == "error"
            || e.category == "failure"
            || e.data.as_ref().is_some_and(|d| {
                d.get("error").is_some()
                    || d.get("errors")
                        .and_then(|v| v.as_array())
                        .is_some_and(|a| !a.is_empty())
            })
    })
}

fn run_has_success(run: &[TimelineEvent]) -> bool {
    run.iter().any(|e| {
        e.category == "success"
            || e.category == "done"
            || e.data.as_ref().is_some_and(|d| d.get("success").is_some())
    })
}

fn run_has_user_corrections(run: &[TimelineEvent]) -> bool {
    run.iter().any(|e| e.category == "user_correction")
}

fn summarize_run(run: &[TimelineEvent]) -> Result<String> {
    let summary = run
        .iter()
        .map(|e| {
            format!(
                "{} [{}] {}",
                e.timestamp.to_rfc3339(),
                e.category,
                e.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if summary.len() > MAX_REFINEMENT_SOURCE_BYTES {
        bail!(
            "qualifying refinement trajectory exceeds the {MAX_REFINEMENT_SOURCE_BYTES} byte source limit"
        );
    }
    Ok(summary)
}

fn extract_json(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix("```json")
        .and_then(|s| s.rfind("```").map(|i| &s[..i]))
    {
        inner.trim().to_string()
    } else if let Some(inner) = trimmed
        .strip_prefix("```")
        .and_then(|s| s.rfind("```").map(|i| &s[..i]))
    {
        inner.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

async fn llm_generate(prompt: &str) -> Result<String> {
    const MAX_GENERATOR_OUTPUT_BYTES: usize = 64 * 1024;
    const GENERATOR_TIMEOUT: Duration = Duration::from_secs(120);
    let prompt_file = crate::write_prompt_temp(prompt).await?;
    let _guard = crate::PromptFileGuard(prompt_file.clone());
    let exe = std::env::current_exe()?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("exec")
        .arg("--prompt-file")
        .arg(&prompt_file)
        .arg("--disallowed-tools")
        .arg(crate::all_tool_ids_csv())
        .env_remove("OMGB_AUTO_SKILL")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (mut child, group) = crate::spawn_with_process_group(cmd)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("skill generator stdout was not piped"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("skill generator stderr was not piped"))?;
    let stdout_task = tokio::spawn(async move {
        let mut capture = crate::BoundedCapture::new(MAX_GENERATOR_OUTPUT_BYTES + 1);
        tokio::io::copy(&mut stdout, &mut capture).await?;
        Ok::<_, std::io::Error>(capture.into_string())
    });
    let stderr_task = tokio::spawn(async move {
        let mut capture = crate::BoundedCapture::new(MAX_GENERATOR_OUTPUT_BYTES + 1);
        tokio::io::copy(&mut stderr, &mut capture).await?;
        Ok::<_, std::io::Error>(capture.into_string())
    });
    let status = match tokio::time::timeout(GENERATOR_TIMEOUT, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            crate::kill_child_and_reap(&mut child, group.as_ref()).await;
            stdout_task.abort();
            stderr_task.abort();
            bail!(
                "skill generation timed out after {}s",
                GENERATOR_TIMEOUT.as_secs()
            );
        }
    };
    crate::kill_process_group(group.as_ref());
    let stdout = stdout_task.await??;
    let stderr = stderr_task.await??;
    if stdout.len() > MAX_GENERATOR_OUTPUT_BYTES || stderr.len() > MAX_GENERATOR_OUTPUT_BYTES {
        bail!("skill generator output exceeds the {MAX_GENERATOR_OUTPUT_BYTES} byte limit");
    }
    if !status.success() {
        bail!("skill generation failed: {stderr}");
    }
    Ok(stdout)
}

/// Read `timeline.jsonl` and, if a run had >= `threshold` tool calls and either
/// (errors + eventual success) or explicit user corrections, ask the LLM to
/// generate a reusable `Skill`.
fn run_qualifies_for_skill(run: &[TimelineEvent], threshold: usize) -> bool {
    let tool_calls: usize = run.iter().map(tool_call_count).sum();
    tool_calls >= threshold
        && ((run_has_errors(run) && run_has_success(run)) || run_has_user_corrections(run))
}

pub async fn propose_skill_from_timeline(threshold: usize) -> Result<Option<RefinementProposal>> {
    let path = crate::providers::omg_dir()?.join("timeline.jsonl");
    if !path.exists() {
        return Ok(None);
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
    events.sort_by_key(|e| e.timestamp);

    let runs = group_runs(events);
    let mut candidate: Option<&[TimelineEvent]> = None;
    for run in runs.iter().rev() {
        if run_qualifies_for_skill(run, threshold) {
            candidate = Some(run);
            break;
        }
    }

    let Some(run) = candidate else {
        return Ok(None);
    };
    let source = summarize_run(run)?;

    let prompt = format!(
        "The following is a timeline of an `omgb` run that used many tool calls, encountered errors and eventually succeeded, \
         or required explicit user corrections. Create a concise, reusable skill that would help avoid the errors, \
         apply the corrections, and complete the task faster.\n\n{}\n\n\
         Return a JSON object with fields: name, trigger, steps (list of strings), pitfalls (list of strings), verification (list of strings). \
         The trigger should be a short path or keyword substring that identifies when this skill applies (e.g. \"crates/oh-my-grok-build\" or \"rust\").",
        source
    );

    let raw = llm_generate(&prompt).await?;
    let json = extract_json(&raw);
    let skill: Skill = serde_json::from_str(&json)
        .map_err(|e| anyhow::anyhow!("failed to parse generated skill: {e}\n{json}"))?;
    validate_skill(&skill)?;
    Ok(Some(propose_skill(skill, &source)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_skill_markdown_roundtrip() {
        let skill = Skill {
            name: "Rust Refactor".into(),
            trigger: "crates/oh-my-grok-build".into(),
            steps: vec!["Run cargo clippy".into(), "Fix warnings".into()],
            pitfalls: vec!["Do not break tests".into()],
            verification: vec!["cargo test passes".into()],
            path: PathBuf::new(),
        };
        let md = format_skill_markdown(&skill).unwrap();
        let parsed = parse_skill_markdown(&md).unwrap();
        assert_eq!(parsed.name, skill.name);
        assert_eq!(parsed.trigger, skill.trigger);
        assert_eq!(parsed.steps, skill.steps);
        assert_eq!(parsed.pitfalls, skill.pitfalls);
        assert_eq!(parsed.verification, skill.verification);
    }

    #[test]
    fn test_trigger_matches() {
        assert!(trigger_matches("C:\\Users\\foo\\src", "src"));
        assert!(trigger_matches(
            "/home/foo/crates/oh-my-grok-build",
            "crates/oh-my-grok-build"
        ));
        assert!(!trigger_matches("/home/foo/bar", "baz"));
        assert!(trigger_matches("/any/cwd", ""));
    }

    #[test]
    fn test_extract_json() {
        let raw = "```json\n{\"name\":\"x\"}\n```";
        assert_eq!(extract_json(raw), "{\"name\":\"x\"}");
        let raw2 = "{\"name\":\"y\"}";
        assert_eq!(extract_json(raw2), "{\"name\":\"y\"}");
    }

    #[test]
    fn test_run_grouping_and_selection() {
        let t = Utc::now();
        let events = vec![
            TimelineEvent {
                timestamp: t,
                category: "exec".into(),
                message: "fix bug".into(),
                data: None,
            },
            TimelineEvent {
                timestamp: t,
                category: "tool".into(),
                message: "read".into(),
                data: Some(serde_json::json!({"tool_calls": 3})),
            },
            TimelineEvent {
                timestamp: t,
                category: "error".into(),
                message: "compile fail".into(),
                data: None,
            },
            TimelineEvent {
                timestamp: t,
                category: "tool".into(),
                message: "edit".into(),
                data: Some(serde_json::json!({"tool_calls": 2})),
            },
            TimelineEvent {
                timestamp: t,
                category: "success".into(),
                message: "done".into(),
                data: None,
            },
        ];
        let runs = group_runs(events);
        assert_eq!(runs.len(), 1);
        let tool_calls: usize = runs[0].iter().map(tool_call_count).sum();
        assert!(tool_calls >= 4);
        assert!(run_has_errors(&runs[0]));
        assert!(run_has_success(&runs[0]));
    }

    #[test]
    fn refinement_requires_approval_and_rolls_back_without_clobbering() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home =
            std::env::temp_dir().join(format!("omgb-refinement-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));

        let skill = Skill {
            name: "Cache Stable Review".into(),
            trigger: "oh-my-grok-build".into(),
            steps: vec!["Keep stable policy before per-turn context".into()],
            pitfalls: vec!["Do not mutate the base prompt".into()],
            verification: vec!["Compare the stable prefix hash".into()],
            path: PathBuf::new(),
        };
        let proposal = propose_skill(skill, "verified trajectory").unwrap();
        assert_eq!(proposal.status, RefinementStatus::Proposed);
        assert!(approve_proposal(&proposal.id, false).is_err());

        let active = approve_proposal(&proposal.id, true).unwrap();
        assert_eq!(active.status, RefinementStatus::Active);
        let path = skill_path(&active.candidate.name).unwrap();
        assert!(path.is_file());

        let rolled_back = rollback_proposal(&proposal.id, true).unwrap();
        assert_eq!(rolled_back.status, RefinementStatus::RolledBack);
        assert!(!path.exists());

        crate::providers::set_omg_home_for_tests(None);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn rollback_refuses_to_overwrite_post_approval_drift() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "omgb-refinement-drift-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));

        let skill = Skill {
            name: "Drift Guard".into(),
            trigger: "oh-my-grok-build".into(),
            steps: vec!["Apply a focused change".into()],
            pitfalls: vec![],
            verification: vec!["Check the active content hash".into()],
            path: PathBuf::new(),
        };
        let proposal = propose_skill(skill, "trajectory").unwrap();
        let active = approve_proposal(&proposal.id, true).unwrap();
        let path = skill_path(&active.candidate.name).unwrap();
        crate::providers::write_file_atomic(&path, "operator edit", true).unwrap();

        assert!(rollback_proposal(&proposal.id, true).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "operator edit");
        assert_eq!(
            load_proposal(&proposal.id).unwrap().status,
            RefinementStatus::Ambiguous
        );

        crate::providers::set_omg_home_for_tests(None);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn refinement_inputs_are_bounded_before_generation_or_persistence() {
        let oversized = TimelineEvent {
            timestamp: Utc::now(),
            category: "success".into(),
            message: "x".repeat(MAX_REFINEMENT_SOURCE_BYTES + 1),
            data: None,
        };
        assert!(summarize_run(&[oversized]).is_err());

        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "omgb-refinement-bounds-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let proposal = propose_skill(
            Skill {
                name: "Bounded Review".into(),
                trigger: "rust".into(),
                steps: vec!["Review a bounded candidate".into()],
                pitfalls: vec![],
                verification: vec!["Reject oversized metadata".into()],
                path: PathBuf::new(),
            },
            "trajectory",
        )
        .unwrap();
        assert!(
            reject_proposal(
                &proposal.id,
                Some("x".repeat(MAX_REFINEMENT_NOTE_BYTES + 1))
            )
            .is_err()
        );
        assert_eq!(
            load_proposal(&proposal.id).unwrap().status,
            RefinementStatus::Proposed
        );

        crate::providers::set_omg_home_for_tests(None);
        std::fs::remove_dir_all(home).ok();
    }
}
