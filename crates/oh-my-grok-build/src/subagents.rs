//! Subagent process registry for `omgb`.

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

fn subagents_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("subagents.jsonl"))
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub depth: u8,
}

fn load_records() -> Result<Vec<SubagentRecord>> {
    let path = subagents_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l).map_err(|e| anyhow::anyhow!("{}: {e}: {l}", path.display()))
        })
        .collect()
}

fn append_record(record: &SubagentRecord) -> Result<()> {
    let path = subagents_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("subagents path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let line = serde_json::to_string(record)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&path)?;
    file.lock_exclusive()?;
    writeln!(file, "{line}")?;
    drop(file);
    crate::providers::restrict_omg_file_permissions(&path)?;
    Ok(())
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

pub async fn spawn(prompt: &str, yolo: bool) -> Result<()> {
    if !yolo {
        bail!("subagent spawn requires --yolo to auto-approve tool use");
    }

    let parent_depth: u8 = std::env::var("OMGB_SUBAGENT_DEPTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let parent_id = std::env::var("OMGB_SUBAGENT_ID")
        .ok()
        .filter(|s| !s.is_empty());

    if parent_depth >= 2 {
        bail!("grandchild subagents cannot spawn further subagents");
    }
    if parent_depth == 1 {
        let current_id = parent_id.as_deref().unwrap_or("");
        let children = load_records()?
            .into_iter()
            .filter(|r| r.parent_id.as_deref() == Some(current_id))
            .count();
        if children >= 5 {
            bail!("subagent has already spawned 5 grandchild subagents");
        }
    }

    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let id = format!(
        "sub-{}-{}",
        Utc::now().timestamp_millis(),
        std::process::id()
    );
    let out_path = log_path(&id, "out")?;
    let err_path = log_path(&id, "err")?;
    std::fs::create_dir_all(&logs_dir()?)?;

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

    let prompt_file = crate::write_prompt_temp(&prompt).await?;
    let prompt_guard = crate::PromptFileGuard(prompt_file.clone());
    let out_file = std::fs::File::create(&out_path)?;
    let err_file = std::fs::File::create(&err_path)?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("exec")
        .arg("--prompt-file")
        .arg(&prompt_file)
        .arg("--prompt-file-own")
        .env("OMGB_SUBAGENT_DEPTH", (parent_depth + 1).to_string())
        .env("OMGB_SUBAGENT_ID", &id)
        .env(
            "OMGB_SUBAGENT_PARENT_ID",
            parent_id.as_deref().unwrap_or(""),
        )
        .kill_on_drop(false)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file));
    if yolo {
        cmd.arg("--yolo");
    }

    let mut child = match crate::spawn_detached(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = tokio::fs::remove_file(&prompt_file).await;
            return Err(anyhow::anyhow!("failed to spawn subagent: {e}"));
        }
    };
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("could not get subagent pid"))?;

    let record = SubagentRecord {
        id: id.clone(),
        pid,
        prompt,
        started_at: Utc::now(),
        command: format!(
            "{exe} exec --prompt-file <prompt>{}",
            if yolo { " --yolo" } else { "" }
        ),
        parent_id: parent_id.clone(),
        depth: parent_depth + 1,
    };
    append_record(&record)?;
    let _ = crate::notifications::push(
        "subagent_spawned",
        serde_json::json!({"subagent_id": id, "parent_id": parent_id, "depth": parent_depth + 1}),
    );
    println!("spawned subagent {id} (pid {pid})");
    tokio::spawn(async move {
        // Keep the prompt file alive until the child has read it, then reap.
        let _guard = prompt_guard;
        let _ = child.wait().await;
    });
    Ok(())
}

pub fn list() -> Result<()> {
    let records = load_records()?;
    if records.is_empty() {
        println!("No subagents recorded.");
    } else {
        for r in records {
            let alive = if crate::process_alive(r.pid) {
                "running"
            } else {
                "exited"
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

async fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let alive = tokio::task::spawn_blocking(move || crate::process_alive(pid))
            .await
            .unwrap_or(true);
        if !alive {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let alive = tokio::task::spawn_blocking(move || crate::process_alive(pid))
        .await
        .unwrap_or(true);
    !alive
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
    let alive = tokio::task::spawn_blocking(move || crate::process_alive(record.pid))
        .await
        .unwrap_or(true);
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
            if wait_for_process_exit(record.pid, Duration::from_millis(200)).await {
                println!("killed subagent {} (pid {})", record.id, record.pid);
                return Ok(());
            }
            bail!(
                "failed to send SIGTERM to subagent {id} (pid {}): {e}",
                record.pid
            );
        }

        if wait_for_process_exit(record.pid, Duration::from_secs(2)).await {
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

        if wait_for_process_exit(record.pid, Duration::from_secs(2)).await {
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

        if wait_for_process_exit(record.pid, Duration::from_secs(2)).await {
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
    let text = tokio::fs::read_to_string(&path).await?;
    print!("{text}");
    Ok(())
}

pub async fn trace(id: &str) -> Result<()> {
    let out = log_path(id, "out")?;
    let err = log_path(id, "err")?;
    if out.exists() {
        println!("-- stdout --");
        print!("{}", tokio::fs::read_to_string(&out).await?);
    }
    if err.exists() {
        println!("-- stderr --");
        print!("{}", tokio::fs::read_to_string(&err).await?);
    }
    Ok(())
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
}
