//! Subagent process registry for `omgb`.

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
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
        .open(&path)?;
    writeln!(file, "{line}")?;
    crate::providers::restrict_env_file_permissions(&path)?;
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
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let id = format!(
        "sub-{}-{}",
        Utc::now().timestamp_millis(),
        std::process::id()
    );
    let out_path = log_path(&id, "out")?;
    let err_path = log_path(&id, "err")?;
    std::fs::create_dir_all(&logs_dir()?)?;

    let prompt_file = crate::write_prompt_temp(prompt).await?;
    let out_file = std::fs::File::create(&out_path)?;
    let err_file = std::fs::File::create(&err_path)?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("exec")
        .arg("--prompt-file")
        .arg(&prompt_file)
        .arg("--prompt-file-own")
        .kill_on_drop(false)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file));
    crate::configure_detached_cmd(&mut cmd);
    if yolo {
        cmd.arg("--yolo");
    }

    let mut child = match cmd.spawn() {
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
        prompt: prompt.to_string(),
        started_at: Utc::now(),
        command: format!(
            "{exe} exec --prompt-file <prompt>{}",
            if yolo { " --yolo" } else { "" }
        ),
    };
    append_record(&record)?;
    println!("spawned subagent {id} (pid {pid})");
    tokio::spawn(async move {
        // Reap the detached child once it exits so it does not become a zombie.
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

pub fn kill(id: &str) -> Result<()> {
    if !is_safe_id(id) {
        bail!("invalid subagent id '{id}'");
    }
    let records = load_records()?;
    let record = records
        .iter()
        .find(|r| r.id == id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("subagent '{id}' not found"))?;
    if !crate::process_alive(record.pid) {
        println!("subagent {} (pid {}) is not running", record.id, record.pid);
        return Ok(());
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-TERM", &record.pid.to_string()])
            .spawn()?;
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new("taskkill")
            .args(["/PID", &record.pid.to_string(), "/F"])
            .spawn()?;
    }
    println!("killed subagent {} (pid {})", record.id, record.pid);
    Ok(())
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
