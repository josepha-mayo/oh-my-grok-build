//! Bounded, privacy-safe local audit records for tool execution.

use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::Serialize;
use sha2::{Digest, Sha256};

const AUDIT_ENV: &str = "OMGB_TOOL_AUDIT_PATH";
const MAX_AUDIT_BYTES: u64 = 32 * 1024 * 1024;
const RETAIN_AUDIT_BYTES: usize = 16 * 1024 * 1024;
const MAX_AUDIT_LINE_BYTES: usize = 64 * 1024;
const AUDIT_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct ToolActionAudit {
    pub phase: &'static str,
    pub call_id: String,
    pub tool_name: String,
    pub tool_kind: String,
    pub read_only: bool,
    pub args_sha256: String,
    pub idempotency_key: String,
    pub policy_decision: &'static str,
    pub outcome: &'static str,
    pub retry_class: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postcondition: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionAudit {
    pub call_id: String,
    pub tool_name: String,
    pub access_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_detail_sha256: Option<String>,
    pub yolo_mode: bool,
    pub auto_approved: bool,
    pub user_prompted: bool,
    pub decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<String>,
}

#[derive(Serialize)]
struct AuditEnvelope<T> {
    schema_version: u8,
    timestamp: String,
    record_type: &'static str,
    data: T,
}

fn audit_path() -> Result<Option<PathBuf>> {
    let Some(raw) = std::env::var_os(AUDIT_ENV) else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        bail!("{AUDIT_ENV} must be an absolute path");
    }
    Ok(Some(path))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical_json(value)))
                    .collect(),
            )
        }
        value => value.clone(),
    }
}

pub fn canonical_json_sha256(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(&canonical_json(value)).unwrap_or_default();
    sha256(&bytes)
}

pub fn text_sha256(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.is_empty())
        .map(|value| sha256(value.as_bytes()))
}

pub fn idempotency_key(call_id: &str, tool_name: &str, args_sha256: &str) -> String {
    sha256(format!("{call_id}\0{tool_name}\0{args_sha256}").as_bytes())
}

fn validate_existing_regular_file(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => bail!("tool audit path is not a regular file: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect tool audit {}", path.display())),
    }
}

fn open_private(path: &Path, append: bool) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("open tool audit {}", path.display()))
}

fn compact_and_append(file: &mut std::fs::File, path: &Path, line: &str) -> Result<()> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read tool audit {}", path.display()))?;
    let mut kept = Vec::new();
    let mut bytes = line.len() + 1;
    let candidates = raw.lines().enumerate().collect::<Vec<_>>();
    for (index, candidate) in candidates.into_iter().rev() {
        serde_json::from_str::<serde_json::Value>(candidate).with_context(|| {
            format!(
                "invalid JSON record in tool audit {} at line {}",
                path.display(),
                index + 1
            )
        })?;
        if bytes.saturating_add(candidate.len() + 1) > RETAIN_AUDIT_BYTES {
            break;
        }
        bytes += candidate.len() + 1;
        kept.push(candidate);
    }
    kept.reverse();
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    for candidate in kept {
        writeln!(file, "{candidate}")?;
    }
    writeln!(file, "{line}")?;
    file.sync_data()?;
    Ok(())
}

fn append_envelope_at<T: Serialize>(path: &Path, record_type: &'static str, data: T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("tool audit path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    validate_existing_regular_file(path)?;
    let lock_path = path.with_extension("jsonl.lock");
    validate_existing_regular_file(&lock_path)?;
    let lock = open_private(&lock_path, false)?;
    lock.lock_exclusive()?;
    validate_existing_regular_file(path)?;
    let envelope = AuditEnvelope {
        schema_version: AUDIT_SCHEMA_VERSION,
        timestamp: chrono::Utc::now().to_rfc3339(),
        record_type,
        data,
    };
    let line = serde_json::to_string(&envelope)?;
    if line.len() > MAX_AUDIT_LINE_BYTES {
        bail!("tool audit record exceeds {MAX_AUDIT_LINE_BYTES} bytes");
    }
    let mut file = open_private(path, true)?;
    let size = file.metadata()?.len();
    let result = if size.saturating_add(line.len() as u64 + 1) > MAX_AUDIT_BYTES {
        compact_and_append(&mut file, path, &line)
    } else {
        writeln!(file, "{line}")?;
        file.sync_data()?;
        Ok(())
    };
    let _ = lock.unlock();
    result
}

fn append_envelope<T: Serialize>(record_type: &'static str, data: T) -> Result<()> {
    let Some(path) = audit_path()? else {
        return Ok(());
    };
    append_envelope_at(&path, record_type, data)
}

pub async fn record_tool_action(record: ToolActionAudit) -> Result<()> {
    tokio::task::spawn_blocking(move || append_envelope("tool_action", record))
        .await
        .context("tool audit writer task failed")?
}

pub fn record_permission(record: PermissionAudit) -> Result<()> {
    append_envelope("permission_decision", record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_hash_ignores_object_key_order() {
        let first = serde_json::json!({"b": 2, "a": {"x": 1}});
        let second = serde_json::json!({"a": {"x": 1}, "b": 2});
        assert_eq!(
            canonical_json_sha256(&first),
            canonical_json_sha256(&second)
        );
    }

    #[test]
    fn idempotency_key_binds_call_tool_and_arguments() {
        let key = idempotency_key("call", "browser", "args");
        assert_eq!(key, idempotency_key("call", "browser", "args"));
        assert_ne!(key, idempotency_key("call", "browser", "other"));
        assert_ne!(key, idempotency_key("other", "browser", "args"));
    }

    #[test]
    fn permission_audit_persists_hashes_without_sensitive_detail() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tool_actions.jsonl");
        let secret = "https://example.test/private?token=secret-value";
        append_envelope_at(
            &path,
            "permission_decision",
            PermissionAudit {
                call_id: "call-1".into(),
                tool_name: "browser".into(),
                access_kind: "web_fetch".into(),
                access_detail_sha256: text_sha256(Some(secret)),
                yolo_mode: false,
                auto_approved: false,
                user_prompted: true,
                decision: "allow".into(),
                prompt_outcome: Some("allow_once".into()),
                permission_mode: Some("ask".into()),
                decision_reason: Some("needs_user".into()),
            },
        )
        .unwrap();
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(!raw.contains(secret));
        assert!(raw.contains("access_detail_sha256"));
        assert!(raw.contains("permission_decision"));
    }
}
