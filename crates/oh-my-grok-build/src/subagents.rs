//! Subagent process registry for `omgb`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const MAX_LOG_DISPLAY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SUBAGENT_STORE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SUBAGENT_RECORDS: usize = 2048;
const MAX_SUBAGENT_PROMPT_BYTES: usize = 256 * 1024;
const MAX_SUBAGENT_LOG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SUBAGENT_LOG_DIRECTORY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ACTIVE_SUBAGENTS: usize = 8;
const SUBAGENT_ADMISSION_TIMEOUT: Duration = Duration::from_secs(30);

fn subagents_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("subagents.jsonl"))
}

fn subagents_lock_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("subagents.lock"))
}

fn logs_dir() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("subagent-logs"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentRecord {
    pub id: String,
    pub pid: u32,
    pub prompt: String,
    pub started_at: DateTime<Utc>,
    pub command: String,
    /// Canonical path of the spawned `omgb` binary.  Older records lack this
    /// binding and are intentionally not eligible for termination: a recycled
    /// PID must never let `subagent kill` target an unrelated process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admission_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub depth: u8,
}

fn load_records() -> Result<Vec<SubagentRecord>> {
    let path = subagents_path()?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "subagent registry is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() > MAX_SUBAGENT_STORE_BYTES {
        bail!("subagent registry exceeds the {MAX_SUBAGENT_STORE_BYTES} byte limit");
    }
    let raw = std::fs::read_to_string(&path)?;
    let mut records = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: SubagentRecord = serde_json::from_str(line).map_err(|error| {
            anyhow::anyhow!("invalid subagent record at line {}: {error}", index + 1)
        })?;
        if record.prompt.len() > MAX_SUBAGENT_PROMPT_BYTES {
            bail!(
                "subagent prompt at line {} exceeds the byte limit",
                index + 1
            );
        }
        records.push(record);
        if records.len() > MAX_SUBAGENT_RECORDS {
            bail!("subagent registry exceeds the {MAX_SUBAGENT_RECORDS} record limit");
        }
    }
    Ok(records)
}

fn open_registry_lock() -> Result<std::fs::File> {
    let path = subagents_lock_path()?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    crate::providers::restrict_omg_file_permissions(&path)?;
    lock.lock_exclusive()?;
    Ok(lock)
}

fn check_spawn_admission(
    records: &[SubagentRecord],
    parent_id: Option<&str>,
    depth: u8,
) -> Result<()> {
    let active = records
        .iter()
        .filter(|candidate| record_may_be_active(candidate))
        .count();
    if active >= MAX_ACTIVE_SUBAGENTS {
        bail!("already running the maximum of {MAX_ACTIVE_SUBAGENTS} subagents");
    }
    if let Some(parent_id) = parent_id
        && depth > 1
        && records
            .iter()
            .filter(|candidate| candidate.parent_id.as_deref() == Some(parent_id))
            .filter(|candidate| record_may_be_active(candidate))
            .count()
            >= 5
    {
        bail!("subagent already has 5 running grandchild subagents");
    }
    Ok(())
}

fn remove_record_logs(id: &str) {
    if !is_safe_id(id) {
        return;
    }
    for ext in ["out", "err", "admit"] {
        if let Ok(path) = log_path(id, ext) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn cleanup_inactive_logs(records: &[SubagentRecord]) -> Result<()> {
    cleanup_inactive_logs_with_limit(records, MAX_SUBAGENT_LOG_DIRECTORY_BYTES)
}

fn cleanup_inactive_logs_with_limit(records: &[SubagentRecord], limit: u64) -> Result<()> {
    let directory = logs_dir()?;
    let mut total = 0_u64;
    let active_ids = records
        .iter()
        .filter(|record| record_may_be_active(record))
        .map(|record| record.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut removable = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.file_type().is_file() {
                continue;
            }
            total = total.saturating_add(metadata.len());
            let id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("");
            let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
            if id.starts_with("sub-")
                && is_safe_id(id)
                && matches!(ext, "out" | "err" | "admit")
                && !active_ids.contains(id)
            {
                removable.push((metadata.modified().ok(), path, metadata.len()));
            }
        }
    }
    if total <= limit {
        return Ok(());
    }
    removable.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (_, path, len) in removable {
        match std::fs::remove_file(&path) {
            Ok(()) => total = total.saturating_sub(len),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                total = total.saturating_sub(len);
            }
            Err(_) => continue,
        }
        if total <= limit {
            break;
        }
    }
    if total > limit {
        bail!("active subagent logs exhaust the aggregate log quota");
    }
    Ok(())
}

fn append_record_locked(records: &mut Vec<SubagentRecord>, record: &SubagentRecord) -> Result<()> {
    if record.prompt.len() > MAX_SUBAGENT_PROMPT_BYTES {
        bail!("subagent prompt exceeds the {MAX_SUBAGENT_PROMPT_BYTES} byte limit");
    }
    let path = subagents_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("subagents path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    crate::providers::restrict_omg_directory_permissions(parent)?;
    records.push(record.clone());
    let mut pruned_ids = Vec::new();
    while records.len() > MAX_SUBAGENT_RECORDS {
        let Some(index) = records
            .iter()
            .position(|candidate| !record_may_be_active(candidate))
        else {
            bail!("too many active subagent records");
        };
        pruned_ids.push(records.remove(index).id);
    }
    loop {
        let mut raw = Vec::new();
        for existing in records.iter() {
            serde_json::to_writer(&mut raw, existing)?;
            raw.push(b'\n');
        }
        if raw.len() as u64 <= MAX_SUBAGENT_STORE_BYTES {
            crate::providers::write_file_atomic(&path, raw, true)?;
            for id in pruned_ids {
                remove_record_logs(&id);
            }
            return cleanup_inactive_logs(records);
        }
        let Some(index) = records
            .iter()
            .position(|candidate| !record_may_be_active(candidate))
        else {
            bail!("active subagent records exceed the {MAX_SUBAGENT_STORE_BYTES} byte limit");
        };
        pruned_ids.push(records.remove(index).id);
    }
}

fn save_record_snapshot_locked(records: &[SubagentRecord]) -> Result<()> {
    let mut raw = Vec::new();
    for record in records {
        serde_json::to_writer(&mut raw, record)?;
        raw.push(b'\n');
    }
    if raw.len() as u64 > MAX_SUBAGENT_STORE_BYTES {
        bail!("subagent registry exceeds the {MAX_SUBAGENT_STORE_BYTES} byte limit");
    }
    crate::providers::write_file_atomic(&subagents_path()?, raw, true)
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '-')
}

fn log_path(id: &str, ext: &str) -> Result<PathBuf> {
    if !is_safe_id(id) {
        bail!("invalid subagent id '{id}'");
    }
    Ok(logs_dir()?.join(format!("{id}.{ext}")))
}

fn is_recorded_subagent_running(record: &SubagentRecord) -> Result<bool> {
    if !crate::process_alive(record.pid) {
        return Ok(false);
    }
    let executable = record.executable.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "subagent '{}' was recorded by an older omgb version without a process identity binding",
            record.id
        )
    })?;
    let actual = crate::lsp::process_image_path(record.pid)?;
    let expected_start = record.process_start.ok_or_else(|| {
        anyhow::anyhow!(
            "subagent '{}' has no process start identity and cannot be safely controlled",
            record.id
        )
    })?;
    Ok(crate::lsp::same_executable(Path::new(executable), &actual)
        && crate::lsp::process_start_identity(record.pid)? == expected_start)
}

fn record_may_be_active(record: &SubagentRecord) -> bool {
    match is_recorded_subagent_running(record) {
        Ok(active) => active,
        Err(_) => crate::process_alive(record.pid),
    }
}

fn current_process_ancestry() -> Result<Vec<u32>> {
    let mut ancestry = Vec::with_capacity(16);
    let mut pid = std::process::id();
    for _ in 0..64 {
        if ancestry.contains(&pid) {
            bail!("process ancestry contains a cycle");
        }
        ancestry.push(pid);
        let Some(parent) = crate::lsp::process_parent_pid(pid)? else {
            break;
        };
        pid = parent;
    }
    Ok(ancestry)
}

fn derive_subagent_lineage_from_ancestry(
    records: &[SubagentRecord],
    ancestry: &[u32],
    claimed_id: Option<&str>,
    claimed_depth: Option<u8>,
) -> Result<(Option<String>, u8)> {
    let mut parent = None;
    for pid in ancestry {
        if let Some(record) = records.iter().find(|record| record.pid == *pid) {
            if !is_recorded_subagent_running(record)? {
                bail!(
                    "subagent ancestry record '{}' is no longer active",
                    record.id
                );
            }
            parent = Some(record);
            break;
        }
    }

    match parent {
        Some(record) => {
            if claimed_id.is_some_and(|id| id != record.id)
                || claimed_depth.is_some_and(|depth| depth != record.depth)
            {
                bail!("subagent lineage environment does not match the recorded ancestor");
            }
            Ok((Some(record.id.clone()), record.depth))
        }
        None => {
            if claimed_id.is_some() || claimed_depth.is_some() {
                bail!("unverified subagent lineage environment");
            }
            Ok((None, 0))
        }
    }
}

fn derive_subagent_lineage(records: &[SubagentRecord]) -> Result<(Option<String>, u8)> {
    let ancestry = current_process_ancestry()?;
    let claimed_id = std::env::var("OMGB_SUBAGENT_ID")
        .ok()
        .filter(|value| !value.is_empty());
    let claimed_depth = std::env::var("OMGB_SUBAGENT_DEPTH")
        .ok()
        .and_then(|value| value.parse::<u8>().ok());
    derive_subagent_lineage_from_ancestry(records, &ancestry, claimed_id.as_deref(), claimed_depth)
}

pub async fn spawn(prompt: &str, yolo: bool) -> Result<()> {
    if !yolo {
        bail!("subagent spawn requires --yolo to auto-approve tool use");
    }
    if prompt.trim().is_empty() || prompt.len() > MAX_SUBAGENT_PROMPT_BYTES {
        bail!("subagent prompt must be non-empty and at most {MAX_SUBAGENT_PROMPT_BYTES} bytes");
    }

    let (parent_id, parent_depth) = tokio::task::spawn_blocking(|| {
        let _lock = open_registry_lock()?;
        derive_subagent_lineage(&load_records()?)
    })
    .await
    .context("subagent lineage task failed")??;

    if parent_depth >= 2 {
        bail!("grandchild subagents cannot spawn further subagents");
    }
    let exe = dunce::canonicalize(std::env::current_exe()?)?;
    let exe_display = exe.to_string_lossy().to_string();
    let id = format!("sub-{}", uuid::Uuid::new_v4());
    let out_path = log_path(&id, "out")?;
    let err_path = log_path(&id, "err")?;
    let admission_path = log_path(&id, "admit")?;
    let admission_token = uuid::Uuid::new_v4().to_string();
    std::fs::create_dir_all(&logs_dir()?)?;
    crate::providers::restrict_omg_directory_permissions(&logs_dir()?)?;

    let prompt = if parent_depth > 0 {
        let spawn_hint = if parent_depth == 1 {
            "You may spawn up to 5 grandchild subagents by running `omgb subagent spawn --yolo \"<task>\"` from a tool."
        } else {
            "You cannot spawn further subagents."
        };
        format!(
            "You are a subagent at nesting depth {}. {spawn_hint}\n\n{prompt}",
            parent_depth + 1
        )
    } else {
        prompt.to_string()
    };
    if prompt.len() > MAX_SUBAGENT_PROMPT_BYTES {
        bail!("expanded subagent prompt exceeds the {MAX_SUBAGENT_PROMPT_BYTES} byte limit");
    }

    let prompt_file = crate::write_prompt_temp(&prompt).await?;
    let prompt_guard = crate::PromptFileGuard(prompt_file.clone());
    let out_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&out_path)?;
    crate::providers::restrict_omg_file_permissions(&out_path)?;
    let err_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&err_path)?;
    crate::providers::restrict_omg_file_permissions(&err_path)?;
    let admission_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&admission_path)?;
    crate::providers::restrict_omg_file_permissions(&admission_path)?;
    drop(out_file);
    drop(err_file);
    drop(admission_file);
    let admission_parent = parent_id.clone();
    let expected_lineage = (parent_id.clone(), parent_depth);
    let admission = tokio::task::spawn_blocking(move || {
        let lock = open_registry_lock()?;
        let records = load_records()?;
        let current_lineage = derive_subagent_lineage(&records)?;
        if current_lineage != expected_lineage {
            bail!("subagent ancestry changed before spawn admission");
        }
        check_spawn_admission(&records, admission_parent.as_deref(), parent_depth + 1)?;
        Ok::<_, anyhow::Error>((lock, records))
    })
    .await
    .context("subagent admission task failed")?;
    let (registry_lock, mut records) = match admission {
        Ok(admission) => admission,
        Err(error) => {
            remove_record_logs(&id);
            return Err(error);
        }
    };
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("subagent")
        .arg("worker")
        .arg("--prompt-file")
        .arg(&prompt_file)
        .arg("--stdout-path")
        .arg(&out_path)
        .arg("--stderr-path")
        .arg(&err_path)
        .arg("--admission-path")
        .arg(&admission_path)
        .arg("--admission-token")
        .arg(&admission_token)
        .env("OMGB_SUBAGENT_DEPTH", (parent_depth + 1).to_string())
        .env("OMGB_SUBAGENT_ID", &id)
        .env(
            "OMGB_SUBAGENT_PARENT_ID",
            parent_id.as_deref().unwrap_or(""),
        )
        .kill_on_drop(false)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if yolo {
        cmd.arg("--yolo");
    }

    let mut child = match crate::spawn_detached(cmd) {
        Ok(c) => c,
        Err(e) => {
            drop(registry_lock);
            let _ = std::fs::remove_file(&prompt_file);
            remove_record_logs(&id);
            return Err(anyhow::anyhow!("failed to spawn subagent: {e}"));
        }
    };
    let identity = (|| -> Result<(u32, u64)> {
        let pid = child
            .id()
            .ok_or_else(|| anyhow::anyhow!("could not get subagent pid"))?;
        Ok((pid, crate::lsp::process_start_identity(pid)?))
    })();
    let (pid, process_start) = match identity {
        Ok(identity) => identity,
        Err(error) => {
            drop(registry_lock);
            let _ = child.kill().await;
            let _ = child.wait().await;
            remove_record_logs(&id);
            return Err(error.context("failed to bind subagent process identity"));
        }
    };

    let record = SubagentRecord {
        id: id.clone(),
        pid,
        prompt,
        started_at: Utc::now(),
        command: format!(
            "{exe_display} exec --prompt-file <prompt>{}",
            if yolo { " --yolo" } else { "" }
        ),
        executable: Some(exe_display),
        process_start: Some(process_start),
        admission_token: Some(admission_token.clone()),
        parent_id: parent_id.clone(),
        depth: parent_depth + 1,
    };
    if let Err(e) = append_record_locked(&mut records, &record) {
        records.retain(|candidate| candidate.id != id);
        let rollback = save_record_snapshot_locked(&records);
        drop(registry_lock);
        let _ = child.kill().await;
        let _ = child.wait().await;
        remove_record_logs(&id);
        if let Err(rollback_error) = rollback {
            bail!(
                "subagent recording failed ({e}) and registry rollback failed ({rollback_error}); the unadmitted worker was terminated"
            );
        }
        bail!("subagent started but could not be recorded and was terminated: {e}");
    }
    let admission_result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&admission_path)?;
        file.write_all(admission_token.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = admission_result {
        records.retain(|candidate| candidate.id != id);
        let rollback = save_record_snapshot_locked(&records);
        drop(registry_lock);
        let _ = child.kill().await;
        let _ = child.wait().await;
        remove_record_logs(&id);
        if let Err(rollback_error) = rollback {
            bail!(
                "subagent admission failed ({error}) and registry rollback failed ({rollback_error}); the unadmitted worker was terminated"
            );
        }
        return Err(error.context("failed to admit recorded subagent"));
    }
    drop(registry_lock);
    // The child owns prompt deletion via --prompt-file-own. Relinquish the
    // parent's guard only after the child is durably recorded, so an immediate
    // CLI exit cannot delete the file before the detached child reads it.
    prompt_guard.disarm();
    let _ = crate::notifications::push(
        "subagent_spawned",
        serde_json::json!({"subagent_id": id, "parent_id": parent_id, "depth": parent_depth + 1}),
    );
    println!("spawned subagent {id} (pid {pid})");
    drop(child);
    Ok(())
}

fn validate_worker_path(path: &Path, expected_parent: &Path, kind: &str) -> Result<()> {
    let canonical = dunce::canonicalize(path)?;
    let parent = canonical
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{kind} path has no parent"))?;
    if parent != expected_parent {
        bail!("invalid subagent {kind} path");
    }
    Ok(())
}

fn verify_worker_registration(
    id: &str,
    admission_token: &str,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<()> {
    if !is_safe_id(id) {
        bail!("invalid subagent worker id");
    }
    let _lock = open_registry_lock()?;
    let records = load_records()?;
    let record = records
        .iter()
        .find(|record| record.id == id)
        .ok_or_else(|| anyhow::anyhow!("subagent worker has no registry admission"))?;
    if record.pid != std::process::id()
        || !is_recorded_subagent_running(record)?
        || !record
            .admission_token
            .as_deref()
            .is_some_and(|expected| crate::group::constant_time_token_eq(expected, admission_token))
        || dunce::canonicalize(stdout_path)? != dunce::canonicalize(log_path(id, "out")?)?
        || dunce::canonicalize(stderr_path)? != dunce::canonicalize(log_path(id, "err")?)?
    {
        bail!("subagent worker registry identity does not match this process");
    }
    Ok(())
}

enum LogCaptureEvent {
    QuotaExceeded,
    Failed(String),
}

async fn copy_capped<R: AsyncRead + Unpin>(
    mut reader: R,
    path: &Path,
    event_tx: tokio::sync::mpsc::Sender<LogCaptureEvent>,
    max_bytes: u64,
) -> Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .await?;
    let mut written = 0_u64;
    let mut buffer = vec![0_u8; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            file.flush().await?;
            return Ok(());
        }
        let remaining = max_bytes.saturating_sub(written) as usize;
        if remaining > 0 {
            let keep = count.min(remaining);
            file.write_all(&buffer[..keep]).await?;
            written += keep as u64;
        }
        if count > remaining || written >= max_bytes {
            file.flush().await?;
            let _ = event_tx.send(LogCaptureEvent::QuotaExceeded).await;
            return Ok(());
        }
    }
}

pub async fn run_worker(
    prompt_file: &Path,
    stdout_path: &Path,
    stderr_path: &Path,
    admission_path: &Path,
    admission_token: &str,
    yolo: bool,
) -> Result<()> {
    let scratch = dunce::canonicalize(crate::scratch_dir()?)?;
    validate_worker_path(prompt_file, &scratch, "prompt")?;
    let name = prompt_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if !name.starts_with("omgb-prompt-") || !name.ends_with(".txt") {
        bail!("invalid subagent prompt path");
    }
    let logs = dunce::canonicalize(logs_dir()?)?;
    validate_worker_path(stdout_path, &logs, "stdout")?;
    validate_worker_path(stderr_path, &logs, "stderr")?;
    validate_worker_path(admission_path, &logs, "admission")?;
    if admission_token.len() != 36
        || !admission_token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        bail!("invalid subagent admission token");
    }
    let prompt_guard = crate::PromptFileGuard(prompt_file.to_path_buf());

    let deadline = tokio::time::Instant::now() + SUBAGENT_ADMISSION_TIMEOUT;
    loop {
        let admitted = tokio::fs::read_to_string(admission_path)
            .await
            .is_ok_and(|value| value == admission_token);
        if admitted {
            let _ = tokio::fs::remove_file(admission_path).await;
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("subagent admission timed out before registry commit");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let worker_id = std::env::var("OMGB_SUBAGENT_ID").unwrap_or_default();
    let stdout_for_verification = stdout_path.to_path_buf();
    let stderr_for_verification = stderr_path.to_path_buf();
    let token_for_verification = admission_token.to_string();
    tokio::task::spawn_blocking(move || {
        verify_worker_registration(
            &worker_id,
            &token_for_verification,
            &stdout_for_verification,
            &stderr_for_verification,
        )
    })
    .await
    .context("verify subagent worker registration task failed")??;

    let exe = dunce::canonicalize(std::env::current_exe()?)?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("exec")
        .arg("--prompt-file")
        .arg(prompt_file)
        .arg("--prompt-file-own")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if yolo {
        cmd.arg("--yolo");
    }
    let (mut child, process_group) = crate::spawn_with_process_group(cmd)?;
    if process_group.is_none() {
        crate::kill_child_and_reap(&mut child, None).await;
        bail!("could not establish subagent process-tree containment");
    }
    prompt_guard.disarm();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("subagent stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("subagent stderr was not piped"))?;
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
    let stdout_path = stdout_path.to_path_buf();
    let stderr_path = stderr_path.to_path_buf();
    let stdout_event_tx = event_tx.clone();
    let out = tokio::spawn(async move {
        let result = copy_capped(
            stdout,
            &stdout_path,
            stdout_event_tx.clone(),
            MAX_SUBAGENT_LOG_BYTES,
        )
        .await;
        if let Err(error) = &result {
            let _ = stdout_event_tx
                .send(LogCaptureEvent::Failed(format!(
                    "stdout capture failed: {error}"
                )))
                .await;
        }
        result
    });
    let stderr_event_tx = event_tx;
    let err = tokio::spawn(async move {
        let result = copy_capped(
            stderr,
            &stderr_path,
            stderr_event_tx.clone(),
            MAX_SUBAGENT_LOG_BYTES,
        )
        .await;
        if let Err(error) = &result {
            let _ = stderr_event_tx
                .send(LogCaptureEvent::Failed(format!(
                    "stderr capture failed: {error}"
                )))
                .await;
        }
        result
    });
    let status = tokio::select! {
        biased;
        status = child.wait() => status?,
        event = event_rx.recv() => {
            crate::kill_child_and_reap(&mut child, process_group.as_ref()).await;
            out.abort();
            err.abort();
            let _ = tokio::join!(out, err);
            match event {
                Some(LogCaptureEvent::QuotaExceeded) => {
                    bail!("subagent log quota exceeded; process tree was terminated")
                }
                Some(LogCaptureEvent::Failed(error)) => {
                    bail!("{error}; subagent process tree was terminated")
                }
                None => bail!("subagent log capture stopped unexpectedly"),
            }
        }
    };
    crate::kill_process_group(process_group.as_ref());
    out.await??;
    err.await??;
    if !status.success() {
        bail!("subagent exited with status {status}");
    }
    Ok(())
}

pub fn list() -> Result<()> {
    let records = load_records()?;
    if records.is_empty() {
        println!("No subagents recorded.");
    } else {
        for r in records {
            let alive = match is_recorded_subagent_running(&r) {
                Ok(true) => "running",
                Ok(false) => "exited",
                Err(_) => "unverified",
            };
            println!(
                "{} (pid {}) {} started {}: {}",
                r.id,
                r.pid,
                alive,
                r.started_at.to_rfc3339(),
                r.prompt
            );
        }
    }
    Ok(())
}

async fn wait_for_process_exit(record: &SubagentRecord, timeout: Duration) -> Result<bool> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let record = record.clone();
        let alive = tokio::task::spawn_blocking(move || is_recorded_subagent_running(&record))
            .await
            .map_err(|error| anyhow::anyhow!("subagent identity check task failed: {error}"))??;
        if !alive {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let record = record.clone();
    let alive = tokio::task::spawn_blocking(move || is_recorded_subagent_running(&record))
        .await
        .map_err(|error| anyhow::anyhow!("subagent identity check task failed: {error}"))??;
    Ok(!alive)
}

pub async fn kill(id: &str) -> Result<()> {
    if !is_safe_id(id) {
        bail!("invalid subagent id '{id}'");
    }
    let records = load_records()?;
    let record = records
        .iter()
        .find(|r| r.id == id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("subagent '{id}' not found"))?;
    let verified_record = record.clone();
    let alive = tokio::task::spawn_blocking(move || is_recorded_subagent_running(&verified_record))
        .await
        .map_err(|e| anyhow::anyhow!("subagent identity check failed: {e}"))??;
    if !alive {
        println!("subagent {} (pid {}) is not running", record.id, record.pid);
        return Ok(());
    }

    if cfg!(unix) {
        // The subagent called setsid, so its PID is also its process-group ID.
        let term_status = tokio::process::Command::new("kill")
            .args(["-TERM", &format!("-{}", record.pid)])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if let Err(e) = term_status {
            // If the process died between the probe and the signal, there is nothing to do.
            if wait_for_process_exit(&record, Duration::from_millis(200)).await? {
                println!("killed subagent {} (pid {})", record.id, record.pid);
                return Ok(());
            }
            bail!(
                "failed to send SIGTERM to subagent {id} (pid {}): {e}",
                record.pid
            );
        }

        if wait_for_process_exit(&record, Duration::from_secs(2)).await? {
            println!("killed subagent {} (pid {})", record.id, record.pid);
            return Ok(());
        }

        // Fall back to SIGKILL if the process ignored SIGTERM.
        let _ = tokio::process::Command::new("kill")
            .args(["-KILL", &format!("-{}", record.pid)])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        if wait_for_process_exit(&record, Duration::from_secs(2)).await? {
            println!("killed subagent {} (pid {})", record.id, record.pid);
            return Ok(());
        }

        bail!(
            "subagent {id} (pid {}) did not terminate after SIGTERM/SIGKILL",
            record.pid
        );
    } else {
        let status = tokio::process::Command::new("taskkill")
            .args(["/PID", &record.pid.to_string(), "/F", "/T"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if let Err(e) = status {
            bail!(
                "taskkill failed for subagent {id} (pid {}): {e}",
                record.pid
            );
        }

        if wait_for_process_exit(&record, Duration::from_secs(2)).await? {
            println!("killed subagent {} (pid {})", record.id, record.pid);
            Ok(())
        } else {
            bail!(
                "subagent {id} (pid {}) did not terminate after taskkill",
                record.pid
            )
        }
    }
}

pub async fn logs(id: &str) -> Result<()> {
    let path = log_path(id, "out")?;
    if !path.exists() {
        bail!("no logs for subagent '{id}'");
    }
    let (text, truncated) = read_log_tail(&path, MAX_LOG_DISPLAY_BYTES).await?;
    if truncated {
        println!("-- showing the last {MAX_LOG_DISPLAY_BYTES} bytes --");
    }
    print!("{text}");
    Ok(())
}

pub async fn trace(id: &str) -> Result<()> {
    let out = log_path(id, "out")?;
    let err = log_path(id, "err")?;
    if out.exists() {
        println!("-- stdout --");
        let (text, truncated) = read_log_tail(&out, MAX_LOG_DISPLAY_BYTES).await?;
        if truncated {
            println!("-- showing the last {MAX_LOG_DISPLAY_BYTES} bytes --");
        }
        print!("{text}");
    }
    if err.exists() {
        println!("-- stderr --");
        let (text, truncated) = read_log_tail(&err, MAX_LOG_DISPLAY_BYTES).await?;
        if truncated {
            println!("-- showing the last {MAX_LOG_DISPLAY_BYTES} bytes --");
        }
        print!("{text}");
    }
    Ok(())
}

async fn read_log_tail(path: &Path, limit: u64) -> Result<(String, bool)> {
    let mut file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    let truncated = len > limit;
    if truncated {
        file.seek(std::io::SeekFrom::Start(len - limit)).await?;
    }
    let mut bytes = Vec::with_capacity(len.min(limit) as usize);
    file.take(limit).read_to_end(&mut bytes).await?;
    if truncated {
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=newline);
        } else {
            bytes.clear();
        }
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_id() {
        assert!(is_safe_id("sub-123-456"));
        assert!(is_safe_id("a.b_c"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id("."));
        assert!(!is_safe_id(".."));
        assert!(!is_safe_id("foo/bar"));
        assert!(!is_safe_id("foo\\bar"));
    }

    #[test]
    fn old_records_are_not_termination_eligible() {
        let record = SubagentRecord {
            id: "sub-1-2".into(),
            pid: 0,
            prompt: "test".into(),
            started_at: Utc::now(),
            command: "omgb exec".into(),
            executable: None,
            process_start: None,
            admission_token: None,
            parent_id: None,
            depth: 1,
        };
        assert!(is_recorded_subagent_running(&record).is_ok_and(|running| !running));
    }

    #[test]
    fn prompt_file_ownership_can_be_transferred_to_the_child() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let path = temp.path().to_path_buf();
        let guard = crate::PromptFileGuard(path.clone());
        guard.disarm();
        assert!(path.exists());
    }

    #[test]
    fn grandchild_admission_is_rejected_before_a_sixth_spawn() {
        let records = (0..5)
            .map(|index| SubagentRecord {
                id: format!("sub-{index}"),
                pid: std::process::id(),
                prompt: "test".into(),
                started_at: Utc::now(),
                command: "omgb exec".into(),
                executable: None,
                process_start: None,
                admission_token: None,
                parent_id: Some("parent".into()),
                depth: 2,
            })
            .collect::<Vec<_>>();
        assert!(check_spawn_admission(&records, Some("parent"), 2).is_err());
    }

    #[test]
    fn recorded_ancestry_cannot_reset_depth_by_clearing_environment() {
        let pid = std::process::id();
        let record = SubagentRecord {
            id: "depth-two-worker".into(),
            pid,
            prompt: "test".into(),
            started_at: Utc::now(),
            command: "omgb exec".into(),
            executable: Some(
                dunce::canonicalize(std::env::current_exe().unwrap())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
            process_start: Some(crate::lsp::process_start_identity(pid).unwrap()),
            admission_token: Some("token".into()),
            parent_id: Some("depth-one-worker".into()),
            depth: 2,
        };
        let lineage = derive_subagent_lineage_from_ancestry(&[record], &[pid], None, None)
            .expect("OS ancestry is authoritative even when inherited variables are cleared");
        assert_eq!(lineage, (Some("depth-two-worker".into()), 2));
    }

    #[test]
    fn aggregate_log_cleanup_removes_oldest_inactive_logs() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "omgb-subagent-log-cleanup-test-{}",
            uuid::Uuid::new_v4()
        ));
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        std::fs::create_dir_all(logs_dir().unwrap()).unwrap();
        let record = |id: &str, seconds: i64| SubagentRecord {
            id: id.into(),
            pid: 0,
            prompt: "test".into(),
            started_at: Utc::now() + chrono::Duration::seconds(seconds),
            command: "omgb exec".into(),
            executable: None,
            process_start: None,
            admission_token: None,
            parent_id: None,
            depth: 1,
        };
        let records = vec![record("sub-old", -10), record("sub-new", 0)];
        let old_path = log_path("sub-old", "out").unwrap();
        let new_path = log_path("sub-new", "out").unwrap();
        std::fs::write(&old_path, b"old!").unwrap();
        std::fs::write(&new_path, b"new!").unwrap();
        let now = std::time::SystemTime::now();
        filetime::set_file_mtime(
            &old_path,
            filetime::FileTime::from_system_time(now - std::time::Duration::from_secs(10)),
        )
        .unwrap();
        filetime::set_file_mtime(&new_path, filetime::FileTime::from_system_time(now)).unwrap();

        cleanup_inactive_logs_with_limit(&records, 4).unwrap();
        assert!(!log_path("sub-old", "out").unwrap().exists());
        assert!(log_path("sub-new", "out").unwrap().exists());

        crate::providers::set_omg_home_for_tests(None);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn capped_log_writer_stops_at_its_quota() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let path = temp.path().to_path_buf();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            copy_capped(&b"too much output"[..], &path, tx, 4)
                .await
                .unwrap();
            assert!(matches!(
                rx.recv().await,
                Some(LogCaptureEvent::QuotaExceeded)
            ));
        });
        assert_eq!(std::fs::read(&path).unwrap(), b"too ");
    }

    #[tokio::test]
    async fn log_tail_is_bounded_and_starts_at_a_line_boundary() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), b"first line\nsecond line\nthird line\n").unwrap();
        let (tail, truncated) = read_log_tail(temp.path(), 14).await.unwrap();
        assert!(truncated);
        assert_eq!(tail, "third line\n");
    }
}
