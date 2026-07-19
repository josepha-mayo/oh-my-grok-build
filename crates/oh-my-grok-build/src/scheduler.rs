//! Background scheduler for `omgb`.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};

fn schedule_path() -> PathBuf {
    crate::providers::omg_dir().join("schedule.jsonl")
}

fn pid_path() -> PathBuf {
    crate::providers::omg_dir().join("scheduler.pid")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub name: String,
    pub expression: String,
    pub prompt: String,
    pub model: Option<String>,
    pub last_run: Option<DateTime<Utc>>,
}

fn load_jobs() -> Result<Vec<ScheduledJob>> {
    let path = schedule_path();
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

fn save_jobs(jobs: &[ScheduledJob]) -> Result<()> {
    let path = schedule_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension(format!("jsonl.tmp.{}", std::process::id()));
    let mut f = std::fs::File::create(&tmp)?;
    for job in jobs {
        writeln!(f, "{}", serde_json::to_string(job)?)?;
    }
    drop(f);
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn list_jobs() -> Result<()> {
    let jobs = load_jobs()?;
    if jobs.is_empty() {
        println!("No scheduled jobs.");
        return Ok(());
    }
    for job in jobs {
        println!("{}: '{}' ({})", job.name, job.prompt, job.expression);
    }
    Ok(())
}

pub fn add_job(
    name: Option<String>,
    expression: &str,
    prompt: &str,
    model: Option<String>,
) -> Result<()> {
    if parse_interval(expression).is_none() && parse_cron(expression).is_err() {
        bail!("invalid schedule expression: {expression}");
    }
    let name = name.unwrap_or_else(|| format!("job-{}", Utc::now().timestamp_millis()));
    let mut jobs = load_jobs()?;
    jobs.retain(|j| j.name != name);
    jobs.push(ScheduledJob {
        name: name.clone(),
        expression: expression.into(),
        prompt: prompt.into(),
        model,
        last_run: None,
    });
    save_jobs(&jobs)?;
    println!("scheduled job '{name}'");
    Ok(())
}

pub fn delete_job(name: &str) -> Result<()> {
    let mut jobs = load_jobs()?;
    let before = jobs.len();
    jobs.retain(|j| j.name != name);
    if jobs.len() == before {
        bail!("job '{name}' not found");
    }
    save_jobs(&jobs)?;
    println!("deleted job '{name}'");
    Ok(())
}

pub async fn run_job(name: &str) -> Result<()> {
    let jobs = load_jobs()?;
    let job = jobs
        .iter()
        .find(|j| j.name == name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("job '{name}' not found"))?;

    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("exec")
        .arg(&job.prompt)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(model) = &job.model {
        cmd.arg("--model").arg(model);
    }
    let status = cmd.status().await?;
    if !status.success() {
        bail!(
            "job '{name}' exited with status {}",
            status.code().unwrap_or(-1)
        );
    }

    let mut jobs = load_jobs()?;
    if let Some(j) = jobs.iter_mut().find(|j| j.name == name) {
        j.last_run = Some(Utc::now());
    }
    save_jobs(&jobs)?;
    Ok(())
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

pub async fn run_daemon_loop() -> Result<()> {
    std::fs::create_dir_all(crate::providers::omg_dir())?;
    let pid = std::process::id();
    std::fs::write(pid_path(), pid.to_string())?;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let jobs = match load_jobs() {
            Ok(j) => j,
            Err(_) => continue,
        };
        for job in jobs {
            if is_due(&job) {
                let _ = run_job(&job.name).await;
                let mut jobs = match load_jobs() {
                    Ok(j) => j,
                    Err(_) => continue,
                };
                if let Some(j) = jobs.iter_mut().find(|j| j.name == job.name) {
                    j.last_run = Some(Utc::now());
                }
                let _ = save_jobs(&jobs);
            }
        }
    }
}

pub fn stop_daemon() -> Result<()> {
    let raw = std::fs::read_to_string(pid_path()).unwrap_or_default();
    let pid = raw
        .trim()
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("no scheduler pid"))?;
    if !process_alive(pid) {
        let _ = std::fs::remove_file(pid_path());
        println!("scheduler is not running");
        return Ok(());
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .spawn()?;
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .spawn()?;
    }
    let _ = std::fs::remove_file(pid_path());
    println!("sent stop to scheduler (pid {pid})");
    Ok(())
}

fn is_due(job: &ScheduledJob) -> bool {
    let now = Local::now();
    let mut due = false;

    if let Some(secs) = parse_interval(&job.expression) {
        due = job
            .last_run
            .map(|t| (Utc::now() - t).num_seconds() >= secs as i64)
            .unwrap_or(true);
    } else if let Ok(fields) = parse_cron(&job.expression) {
        due = cron_matches(&fields, now);
    }
    due
}

fn parse_interval(expr: &str) -> Option<u64> {
    let expr = expr.trim();
    if expr.is_empty() {
        return None;
    }
    let num: String = expr
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let unit: String = expr
        .chars()
        .skip_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let n: f64 = num.parse().ok()?;
    let multiplier = match unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 60 * 60,
        "d" | "day" | "days" => 24 * 60 * 60,
        _ => return None,
    };
    Some((n * multiplier as f64) as u64)
}

fn parse_cron(expr: &str) -> Result<Vec<Vec<String>>> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        bail!("cron expression must have 5 fields");
    }
    Ok(parts
        .into_iter()
        .map(|p| p.split(',').map(|s| s.to_string()).collect())
        .collect())
}

fn cron_matches(fields: &[Vec<String>], now: Local) -> bool {
    let minute = now.minute();
    let hour = now.hour();
    let day = now.day();
    let month = now.month();
    let weekday = now.weekday().num_days_from_sunday();

    matches_field(&fields[0], minute as u32, 0, 59)
        && matches_field(&fields[1], hour as u32, 0, 23)
        && matches_field(&fields[2], day as u32, 1, 31)
        && matches_field(&fields[3], month as u32, 1, 12)
        && matches_field(&fields[4], weekday as u32, 0, 7)
}

fn matches_field(parts: &[String], value: u32, min: u32, max: u32) -> bool {
    parts.iter().any(|p| field_matches(value, p, min, max))
}

fn field_matches(value: u32, token: &str, min: u32, max: u32) -> bool {
    let token = token.trim();
    if token == "*" {
        return true;
    }

    if let Some(suffix) = token.strip_prefix("*/") {
        if let Ok(step) = suffix.parse::<u32>() {
            if step > 0 && value >= min && value <= max {
                return (value - min) % step == 0;
            }
        }
        return false;
    }

    if let Some((range, step_str)) = token.split_once('/') {
        let step = step_str.trim().parse::<u32>().unwrap_or(1);
        if step == 0 {
            return false;
        }
        if let Some((start, end)) = range.split_once('-') {
            if let (Ok(s), Ok(e)) = (start.trim().parse::<u32>(), end.trim().parse::<u32>()) {
                let (lo, hi) = if s <= e { (s, e) } else { (e, s) };
                let lo = lo.clamp(min, max);
                let hi = hi.clamp(min, max);
                return value >= lo && value <= hi && (value - lo) % step == 0;
            }
        }
        return false;
    }

    if let Some((start, end)) = token.split_once('-') {
        if let (Ok(s), Ok(e)) = (start.trim().parse::<u32>(), end.trim().parse::<u32>()) {
            let (lo, hi) = if s <= e { (s, e) } else { (e, s) };
            return value >= lo && value <= hi;
        }
        return false;
    }

    token.parse::<u32>().is_ok_and(|n| n == value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_interval() {
        assert_eq!(parse_interval("30s"), Some(30));
        assert_eq!(parse_interval("5m"), Some(300));
        assert_eq!(parse_interval("2h"), Some(7200));
        assert_eq!(parse_interval("1d"), Some(86400));
        assert_eq!(parse_interval("foo"), None);
    }

    #[test]
    fn test_parse_cron() {
        let fields = parse_cron("0 9 * * *").unwrap();
        assert_eq!(fields.len(), 5);
        assert_eq!(fields[0], vec!["0"]);
        assert_eq!(fields[1], vec!["9"]);
    }

    #[test]
    fn test_matches_field() {
        assert!(matches_field(&["*".to_string()], 42, 0, 59));
        assert!(matches_field(&["5".to_string()], 5, 0, 59));
        assert!(!matches_field(&["5".to_string()], 6, 0, 59));
    }
}
