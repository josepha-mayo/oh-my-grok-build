//! Subagent process registry for `omgb`.

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

fn subagents_path() -> PathBuf {
    crate::providers::omg_dir().join("subagents.jsonl")
}

fn logs_dir() -> PathBuf {
    crate::providers::omg_dir().join("subagent-logs")
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
    let path = subagents_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

fn append_record(record: &SubagentRecord) -> Result<()> {
    let path = subagents_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let line = serde_json::to_string(record)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{line}")?;
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
    Ok(logs_dir().join(format!("{id}.{ext}")))
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    let Ok(output) = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.contains(&pid.to_string())
}

#[cfg(not(any(unix, windows)))]
fn process_alive(_pid: u32) -> bool {
    true
}

pub async fn spawn(prompt: &str) -> Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let id = format!(
        "sub-{}-{}",
        Utc::now().timestamp_millis(),
        std::process::id()
    );
    let out_path = log_path(&id, "out")?;
    let err_path = log_path(&id, "err")?;
    std::fs::create_dir_all(&logs_dir())?;

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("exec")
        .arg(prompt)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("could not get subagent pid"))?;

    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut out_file = tokio::fs::File::create(&out_path).await?;
    let mut err_file = tokio::fs::File::create(&err_path).await?;

    tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stdout, &mut out_file).await;
        let _ = out_file.flush().await;
    });
    tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stderr, &mut err_file).await;
        let _ = err_file.flush().await;
    });
    tokio::spawn(async move {
        let mut child = child;
        let _ = child.wait().await;
    });

    let record = SubagentRecord {
        id: id.clone(),
        pid,
        prompt: prompt.to_string(),
        started_at: Utc::now(),
        command: format!("{exe} exec <prompt>"),
    };
    append_record(&record)?;
    println!("spawned subagent {id} (pid {pid})");
    Ok(())
}

pub fn list() -> Result<()> {
    let records = load_records()?;
    if records.is_empty() {
        println!("No subagents recorded.");
    } else {
        for r in records {
            let alive = if process_alive(r.pid) {
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
    if !process_alive(record.pid) {
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
