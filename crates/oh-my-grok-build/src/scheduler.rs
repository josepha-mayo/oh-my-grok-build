//! Background scheduler for `omgb`.

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{DateTime, Local, Utc};
use croner::Cron;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DAEMON_POLL_INTERVAL: Duration = Duration::from_secs(60);
const DEFAULT_JOB_TIMEOUT: Duration = Duration::from_secs(300);

fn schedule_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("schedule.jsonl"))
}

fn schedule_lock_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("schedule.lock"))
}

fn pid_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("scheduler.pid"))
}

/// Holds the scheduler PID file open with an exclusive `fs2` lock so only one
/// daemon runs at a time. The lock is released when this value is dropped.
struct PidFile {
    _file: std::fs::File,
}

impl PidFile {
    fn acquire() -> Result<Self> {
        let path = pid_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        if let Err(e) = file.try_lock_exclusive() {
            bail!("scheduler daemon is already running: {e}");
        }
        file.set_len(0)?;
        let mut file = file;
        writeln!(file, "{}", std::process::id())?;
        Ok(PidFile { _file: file })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Expiry {
    Ts(i64),
    Iso(DateTime<Utc>),
}

impl Expiry {
    fn as_datetime(self) -> Option<DateTime<Utc>> {
        match self {
            Expiry::Ts(ts) => DateTime::from_timestamp(ts, 0),
            Expiry::Iso(dt) => Some(dt),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub name: String,
    pub expression: String,
    pub prompt: String,
    pub model: Option<String>,
    #[serde(default)]
    pub yolo: bool,
    pub last_run: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<Expiry>,
}

/// Acquire an exclusive file lock on `schedule.lock`.
/// The lock is released when the returned `File` is dropped.
async fn lock_schedule() -> Result<std::fs::File> {
    let path = schedule_lock_path()?;
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| anyhow::anyhow!("failed to open schedule lock: {e}"))?;
        file.lock_exclusive()
            .map_err(|e| anyhow::anyhow!("failed to lock schedule: {e}"))?;
        Ok(file)
    })
    .await
    .map_err(|e| anyhow::anyhow!("schedule lock task panicked: {e}"))?
}

fn load_jobs() -> Result<Vec<ScheduledJob>> {
    let path = schedule_path()?;
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

fn save_jobs(jobs: &[ScheduledJob]) -> Result<()> {
    let path = schedule_path()?;
    let mut content = String::new();
    for job in jobs {
        content.push_str(&serde_json::to_string(job)?);
        content.push('\n');
    }
    crate::providers::write_file_atomic(&path, content, true)
}

async fn with_jobs<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&mut Vec<ScheduledJob>) -> Result<R>,
{
    let _lock = lock_schedule().await?;
    let mut jobs = load_jobs()?;
    let result = f(&mut jobs)?;
    save_jobs(&jobs)?;
    Ok(result)
}

async fn with_jobs_read<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&[ScheduledJob]) -> Result<R>,
{
    let _lock = lock_schedule().await?;
    let jobs = load_jobs()?;
    f(&jobs)
}

pub async fn list_jobs() -> Result<()> {
    with_jobs_read(|jobs| {
        if jobs.is_empty() {
            println!("No scheduled jobs.");
            return Ok(());
        }
        for job in jobs {
            println!("{}: '{}' ({})", job.name, job.prompt, job.expression);
        }
        Ok(())
    })
    .await
}

pub async fn add_job(
    name: Option<String>,
    expression: &str,
    prompt: &str,
    model: Option<String>,
    yolo: bool,
) -> Result<()> {
    if !yolo {
        bail!("scheduled jobs require --yolo to auto-approve tool use");
    }
    if parse_interval(expression).is_none() && Cron::from_str(expression).is_err() {
        bail!("invalid schedule expression: {expression}");
    }
    let name = name.unwrap_or_else(|| format!("job-{}", Utc::now().timestamp_millis()));
    with_jobs(|jobs| {
        jobs.retain(|j| j.name != name);
        jobs.push(ScheduledJob {
            name: name.clone(),
            expression: expression.into(),
            prompt: prompt.into(),
            model,
            yolo,
            last_run: None,
            expires_at: None,
        });
        println!("scheduled job '{name}'");
        Ok(())
    })
    .await
}

pub async fn delete_job(name: &str) -> Result<()> {
    with_jobs(|jobs| {
        let before = jobs.len();
        jobs.retain(|j| j.name != name);
        if jobs.len() == before {
            bail!("job '{name}' not found");
        }
        println!("deleted job '{name}'");
        Ok(())
    })
    .await
}

pub async fn run_job(name: &str, capture: bool) -> Result<()> {
    let job = {
        let _lock = lock_schedule().await?;
        let mut jobs = load_jobs()?;
        let idx = jobs
            .iter()
            .position(|j| j.name == name)
            .ok_or_else(|| anyhow::anyhow!("job '{name}' not found"))?;
        jobs[idx].last_run = Some(Utc::now());
        save_jobs(&jobs)?;
        jobs[idx].clone()
    };
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let prompt_file = crate::write_prompt_temp(&job.prompt).await?;
    let _prompt_guard = crate::PromptFileGuard(prompt_file.clone());

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("exec")
        .arg("--prompt-file")
        .arg(&prompt_file)
        .stdin(Stdio::null());
    if job.yolo {
        cmd.arg("--yolo");
    }
    if let Some(model) = &job.model {
        cmd.arg("--model").arg(model);
    }
    if capture {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    }
    let (mut child, group) = crate::spawn_with_process_group(cmd)?;

    if capture {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("scheduler stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("scheduler stderr was not piped"))?;
        let log_path = crate::providers::omg_dir()?.join("scheduler.log");
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        let writer = tokio::spawn(async move {
            let parent = log_path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("scheduler log path has no parent directory"))?;
            tokio::fs::create_dir_all(parent).await?;
            let mut log = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .await?;
            while let Some(chunk) = rx.recv().await {
                log.write_all(&chunk).await?;
            }
            log.flush().await?;
            Ok::<_, anyhow::Error>(())
        });
        let copy_out = tokio::spawn(copy_stream_to_log_sender(stdout, "stdout", tx.clone()));
        let copy_err = tokio::spawn(copy_stream_to_log_sender(stderr, "stderr", tx));
        let status = match tokio::time::timeout(DEFAULT_JOB_TIMEOUT, child.wait()).await {
            Ok(s) => s?,
            Err(_) => {
                crate::kill_child_and_reap(&mut child, group.as_ref()).await;
                copy_out.abort();
                copy_err.abort();
                writer.abort();
                let _ = tokio::time::timeout(Duration::from_secs(5), async {
                    tokio::join!(copy_out, copy_err, writer)
                })
                .await;
                bail!(
                    "job '{name}' timed out after {}s",
                    DEFAULT_JOB_TIMEOUT.as_secs()
                );
            }
        };
        crate::kill_process_group(group.as_ref());
        let (c_out, c_err, w) = tokio::join!(copy_out, copy_err, writer);
        c_out??;
        c_err??;
        w??;
        if !status.success() {
            Err(anyhow::anyhow!(
                "job '{name}' exited with status {}",
                status.code().unwrap_or(-1)
            ))
        } else {
            Ok(())
        }
    } else {
        let status = match tokio::time::timeout(DEFAULT_JOB_TIMEOUT, child.wait()).await {
            Ok(s) => s?,
            Err(_) => {
                crate::kill_child_and_reap(&mut child, group.as_ref()).await;
                bail!(
                    "job '{name}' timed out after {}s",
                    DEFAULT_JOB_TIMEOUT.as_secs()
                );
            }
        };
        crate::kill_process_group(group.as_ref());
        if !status.success() {
            Err(anyhow::anyhow!(
                "job '{name}' exited with status {}",
                status.code().unwrap_or(-1)
            ))
        } else {
            Ok(())
        }
    }
}

pub async fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("schedule")
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    crate::spawn_detached(cmd)?;

    let path = pid_path()?;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(raw) = std::fs::read_to_string(&path)
            && let Ok(pid) = raw.trim().parse::<u32>()
            && crate::process_alive(pid)
        {
            println!("scheduler daemon started (pid {pid})");
            return Ok(());
        }
    }
    bail!("scheduler daemon failed to start")
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sig = signal(SignalKind::terminate())?;
        sig.recv().await;
        Ok(())
    }
    #[cfg(windows)]
    {
        tokio::signal::ctrl_c().await?;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::future::pending().await
    }
}

pub async fn run_daemon_loop() -> Result<()> {
    let _pid_file = PidFile::acquire()?;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(DAEMON_POLL_INTERVAL) => {}
            _ = shutdown_signal() => {
                println!("scheduler: received shutdown signal");
                return Ok(());
            }
        }
        let (due_jobs, expired): (Vec<ScheduledJob>, usize) = with_jobs_read(|jobs| {
            let now = Utc::now();
            let due: Vec<_> = jobs.iter().filter(|j| is_due(j)).cloned().collect();
            let expired = jobs.iter().filter(|j| is_expired(j, now)).count();
            Ok((due, expired))
        })
        .await?;
        if expired > 0 {
            with_jobs(|jobs| {
                let now = Utc::now();
                jobs.retain(|j| !is_expired(j, now));
                Ok(())
            })
            .await?;
        }
        let mut set = tokio::task::JoinSet::new();
        for job in due_jobs {
            set.spawn(async move { run_job(&job.name, true).await });
        }
        while let Some(res) = set.join_next().await {
            match res {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("scheduler: job error: {e}"),
                Err(e) => eprintln!("scheduler: job task panicked: {e}"),
            }
        }
    }
}

pub async fn omgb_schedule_cleanup_expired() -> Result<usize> {
    with_jobs(|jobs| {
        let now = Utc::now();
        let before = jobs.len();
        jobs.retain(|j| !is_expired(j, now));
        Ok(before - jobs.len())
    })
    .await
}

pub async fn omgb_schedule_set_expiry(id: &str, expires_at: Option<&str>) -> Result<()> {
    let expiry = parse_expiry(expires_at)?;
    with_jobs(|jobs| {
        let job = jobs
            .iter_mut()
            .find(|j| j.name == id)
            .ok_or_else(|| anyhow::anyhow!("job '{id}' not found"))?;
        job.expires_at = expiry;
        Ok(())
    })
    .await
}

pub fn stop_daemon() -> Result<()> {
    let path = pid_path()?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read scheduler pid file: {e}"))?;
    let pid = raw
        .trim()
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("scheduler pid file does not contain a valid pid"))?;
    if !crate::process_alive(pid) {
        let _ = std::fs::remove_file(&path);
        println!("scheduler is not running");
        return Ok(());
    }
    #[cfg(unix)]
    {
        let kill = which::which("kill").unwrap_or_else(|_| PathBuf::from("/bin/kill"));
        std::process::Command::new(kill)
            .args(["-TERM", &pid.to_string()])
            .spawn()?;
    }
    #[cfg(not(unix))]
    {
        let taskkill = which::which("taskkill")
            .unwrap_or_else(|_| PathBuf::from(r"C:\Windows\System32\taskkill.exe"));
        std::process::Command::new(taskkill)
            .args(["/PID", &pid.to_string(), "/F"])
            .spawn()?;
    }
    let _ = std::fs::remove_file(pid_path()?);
    println!("sent stop to scheduler (pid {pid})");
    Ok(())
}

fn is_expired(job: &ScheduledJob, now: DateTime<Utc>) -> bool {
    job.expires_at
        .and_then(|e| e.as_datetime())
        .is_some_and(|e| now >= e)
}

fn parse_expiry(raw: Option<&str>) -> Result<Option<Expiry>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let raw = raw.trim();
    if let Ok(ts) = raw.parse::<i64>() {
        if DateTime::from_timestamp(ts, 0).is_none() {
            bail!("expiry timestamp out of range: {raw}");
        }
        return Ok(Some(Expiry::Ts(ts)));
    }
    match raw.parse::<DateTime<Utc>>() {
        Ok(dt) => Ok(Some(Expiry::Iso(dt))),
        Err(e) => bail!("invalid expiry '{raw}': {e}"),
    }
}

fn is_due(job: &ScheduledJob) -> bool {
    if is_expired(job, Utc::now()) {
        return false;
    }
    if let Some(secs) = parse_interval(&job.expression) {
        return job
            .last_run
            .map(|t| (Utc::now() - t).num_seconds() >= secs as i64)
            .unwrap_or(true);
    }

    let Ok(cron) = Cron::from_str(&job.expression) else {
        return false;
    };

    let now = Local::now();
    if let Some(last) = job.last_run {
        match cron.find_next_occurrence(&last.with_timezone(&Local), false) {
            Ok(next) => now >= next,
            Err(_) => false,
        }
    } else {
        true
    }
}

fn parse_interval(expr: &str) -> Option<u64> {
    let expr = expr.trim();
    if expr.is_empty() {
        return None;
    }
    let mut total: f64 = 0.0;
    let mut s = expr;
    while !s.is_empty() {
        let num_end = s
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len());
        let num = s[..num_end].parse::<f64>().ok()?;
        s = s[num_end..].trim_start();
        let unit_end = s
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(s.len());
        if unit_end == 0 {
            return None;
        }
        let unit = s[..unit_end].to_ascii_lowercase();
        let multiplier = match unit.as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 60 * 60,
            "d" | "day" | "days" => 24 * 60 * 60,
            _ => return None,
        };
        total += num * multiplier as f64;
        s = s[unit_end..].trim_start();
    }
    let secs = total as u64;
    if secs == 0 {
        return None;
    }
    Some(secs)
}

async fn copy_stream_to_log_sender<R: tokio::io::AsyncRead + Unpin>(
    mut stream: R,
    label: &'static str,
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                tx.send(buf[..n].to_vec()).await?;
            }
            Err(e) => bail!("failed to read scheduler {label}: {e}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeDelta, TimeZone};

    #[test]
    fn test_parse_interval() {
        assert_eq!(parse_interval("30s"), Some(30));
        assert_eq!(parse_interval("5m"), Some(300));
        assert_eq!(parse_interval("2h"), Some(7200));
        assert_eq!(parse_interval("1d"), Some(86400));
        assert_eq!(parse_interval("1h30m"), Some(5400));
        assert_eq!(parse_interval(" 90 M "), Some(5400));
        assert_eq!(parse_interval("foo"), None);
    }

    #[test]
    fn test_cron_parsing() {
        assert!(Cron::from_str("0 9 * * *").is_ok());
        assert!(Cron::from_str("* * * * *").is_ok());
        assert!(Cron::from_str("invalid").is_err());
    }

    #[test]
    fn test_cron_next_occurrence() {
        let cron = Cron::from_str("0 0 1 * *").unwrap();
        let start = Local.with_ymd_and_hms(2024, 1, 15, 0, 0, 0).unwrap();
        let next = cron.find_next_occurrence(&start, false).unwrap();
        assert!(next > start);
        assert_eq!(next.day(), 1);
    }

    #[test]
    fn test_is_due_interval() {
        let mut job = ScheduledJob {
            name: "t".into(),
            expression: "60s".into(),
            prompt: "".into(),
            model: None,
            yolo: false,
            last_run: Some(Utc::now() - TimeDelta::seconds(90)),
            expires_at: None,
        };
        assert!(is_due(&job));
        job.last_run = Some(Utc::now() - TimeDelta::seconds(30));
        assert!(!is_due(&job));
        job.last_run = None;
        assert!(is_due(&job));
    }

    #[test]
    fn test_is_due_cron_never_run() {
        let job = ScheduledJob {
            name: "t".into(),
            expression: "* * * * *".into(),
            prompt: "".into(),
            model: None,
            yolo: false,
            last_run: None,
            expires_at: None,
        };
        assert!(is_due(&job));
    }

    #[test]
    fn test_is_expired_and_parse_expiry() {
        let past = Utc::now() - TimeDelta::seconds(10);
        let future = Utc::now() + TimeDelta::seconds(10);
        let mut job = ScheduledJob {
            name: "t".into(),
            expression: "60s".into(),
            prompt: "".into(),
            model: None,
            yolo: false,
            last_run: None,
            expires_at: None,
        };
        assert!(!is_expired(&job, Utc::now()));
        assert!(is_due(&job));
        job.expires_at = Some(Expiry::Ts(past.timestamp()));
        assert!(is_expired(&job, Utc::now()));
        assert!(!is_due(&job));
        job.expires_at = Some(Expiry::Iso(future));
        assert!(!is_expired(&job, Utc::now()));

        assert!(parse_expiry(None).unwrap().is_none());
        assert!(parse_expiry(Some("")).unwrap().is_none());
        assert!(matches!(
            parse_expiry(Some("1700000000")).unwrap().unwrap(),
            Expiry::Ts(1700000000)
        ));
        assert!(
            parse_expiry(Some("2024-01-01T00:00:00Z"))
                .unwrap()
                .is_some()
        );
        assert!(parse_expiry(Some("invalid")).is_err());
    }
}
