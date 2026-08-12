//! Background scheduler for `omgb`.

use std::collections::HashSet;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, Utc};
use croner::Cron;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

const DAEMON_POLL_INTERVAL: Duration = Duration::from_secs(60);
const DEFAULT_JOB_TIMEOUT: Duration = Duration::from_secs(300);
const STALE_JOB_CLAIM_AFTER: Duration = Duration::from_secs(360);
const MAX_SCHEDULE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SCHEDULER_LOG_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SCHEDULE_JOBS: usize = 4096;
const MAX_CONCURRENT_SCHEDULE_JOBS: usize = 4;
const MAX_JOB_NAME_BYTES: usize = 128;
const MAX_SCHEDULE_EXPRESSION_BYTES: usize = 256;
const MAX_SCHEDULE_PROMPT_BYTES: usize = 512 * 1024;
const MAX_SCHEDULE_MODEL_BYTES: usize = 256;

fn schedule_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("schedule.jsonl"))
}

fn schedule_lock_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("schedule.lock"))
}

fn pid_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("scheduler.pid"))
}

fn stop_request_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("scheduler.stop"))
}

/// Holds the scheduler PID file open with an exclusive `fs2` lock so only one
/// daemon runs at a time. The lock is released when this value is dropped.
struct PidFile {
    _file: std::fs::File,
}

#[derive(Debug, Serialize, Deserialize)]
struct SchedulerPidRecord {
    pid: u32,
    executable: String,
    process_start: u64,
}

fn read_scheduler_pid(path: &Path) -> Result<SchedulerPidRecord> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read scheduler pid file: {e}"))?;
    parse_scheduler_pid(&raw)
}

fn parse_scheduler_pid(raw: &str) -> Result<SchedulerPidRecord> {
    if raw.len() > 4096 {
        bail!("scheduler pid file is too large");
    }
    serde_json::from_str(raw).map_err(|_| {
        anyhow::anyhow!(
            "scheduler pid file has no process identity binding; restart the scheduler before stopping it"
        )
    })
}

fn scheduler_process_matches(record: &SchedulerPidRecord) -> Result<bool> {
    if !crate::process_alive(record.pid) {
        return Ok(false);
    }
    let actual = crate::lsp::process_image_path(record.pid)?;
    Ok(
        crate::lsp::same_executable(Path::new(&record.executable), &actual)
            && crate::lsp::process_start_identity(record.pid)? == record.process_start,
    )
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
        if file.try_lock_exclusive().is_err() {
            // Advisory locks are released on process exit/FD close, so a held
            // lock means another scheduler is genuinely running.
            bail!("scheduler daemon is already running");
        }
        let _ = std::fs::remove_file(stop_request_path()?);
        let executable = dunce::canonicalize(std::env::current_exe()?)?;
        let record = SchedulerPidRecord {
            pid: std::process::id(),
            executable: executable.to_string_lossy().into_owned(),
            process_start: crate::lsp::process_start_identity(std::process::id())?,
        };
        let serialized = serde_json::to_vec(&record)?;
        file.set_len(0)?;
        let mut file = file;
        file.write_all(&serialized)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(PidFile { _file: file })
    }
}

fn scheduler_pid_lock_held(path: &Path) -> Result<bool> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    if file.try_lock_exclusive().is_ok() {
        FileExt::unlock(&file)?;
        Ok(false)
    } else {
        Ok(true)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    pub last_run: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<Expiry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<JobRunClaim>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JobRunClaim {
    id: String,
    started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process_start: Option<u64>,
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

fn validate_job_name(name: &str) -> Result<()> {
    if name.len() > MAX_JOB_NAME_BYTES
        || name.trim().is_empty()
        || name.chars().any(char::is_control)
    {
        bail!(
            "job name must be non-empty, contain no controls, and be at most {MAX_JOB_NAME_BYTES} bytes"
        );
    }
    Ok(())
}

fn validate_job(job: &ScheduledJob) -> Result<()> {
    validate_job_name(&job.name)?;
    if job.expression.len() > MAX_SCHEDULE_EXPRESSION_BYTES
        || job.expression.trim().is_empty()
        || job.expression.chars().any(char::is_control)
    {
        bail!(
            "schedule expression must be non-empty, contain no controls, and be at most {MAX_SCHEDULE_EXPRESSION_BYTES} bytes"
        );
    }
    if parse_interval(&job.expression).is_none() && Cron::from_str(&job.expression).is_err() {
        bail!("invalid schedule expression for job '{}'", job.name);
    }
    if job.prompt.len() > MAX_SCHEDULE_PROMPT_BYTES || job.prompt.trim().is_empty() {
        bail!("scheduled prompt must be non-empty and at most {MAX_SCHEDULE_PROMPT_BYTES} bytes");
    }
    if job.prompt.contains('\0') {
        bail!("scheduled prompt must not contain NUL characters");
    }
    if let Some(model) = &job.model
        && (model.len() > MAX_SCHEDULE_MODEL_BYTES
            || model.trim().is_empty()
            || model.chars().any(char::is_control))
    {
        bail!(
            "scheduled model must be non-empty, contain no controls, and be at most {MAX_SCHEDULE_MODEL_BYTES} bytes"
        );
    }
    if job
        .expires_at
        .is_some_and(|expiry| expiry.as_datetime().is_none())
    {
        bail!("job '{}' has an out-of-range expiry", job.name);
    }
    if let Some(claim) = &job.run {
        uuid::Uuid::parse_str(&claim.id).context("scheduled run id must be a UUID")?;
        if claim.pid.is_some() != claim.process_start.is_some() {
            bail!("job '{}' has an incomplete run process identity", job.name);
        }
    }
    Ok(())
}

fn validate_jobs(jobs: &[ScheduledJob]) -> Result<()> {
    if jobs.len() > MAX_SCHEDULE_JOBS {
        bail!("too many scheduled jobs (max {MAX_SCHEDULE_JOBS})");
    }
    let mut names = HashSet::with_capacity(jobs.len());
    for job in jobs {
        validate_job(job)?;
        if !names.insert(&job.name) {
            bail!("duplicate scheduled job name '{}'", job.name);
        }
    }
    Ok(())
}

fn load_jobs() -> Result<Vec<ScheduledJob>> {
    let path = schedule_path()?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => bail!("schedule store is not a regular file: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("inspect schedule store"),
    };
    if metadata.len() > MAX_SCHEDULE_BYTES {
        bail!("schedule store exceeds the {MAX_SCHEDULE_BYTES} byte safety limit");
    }
    let raw = std::fs::read_to_string(&path)?;
    let jobs: Vec<ScheduledJob> = raw
        .lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).with_context(|| {
                format!(
                    "invalid JSON record in schedule store {} at line {}",
                    path.display(),
                    index + 1
                )
            })
        })
        .collect::<Result<_>>()?;
    validate_jobs(&jobs)?;
    Ok(jobs)
}

fn save_jobs(jobs: &[ScheduledJob]) -> Result<()> {
    validate_jobs(jobs)?;
    let path = schedule_path()?;
    let mut content = String::new();
    for job in jobs {
        content.push_str(&serde_json::to_string(job)?);
        content.push('\n');
    }
    if content.len() as u64 > MAX_SCHEDULE_BYTES {
        bail!("schedule store exceeds the {MAX_SCHEDULE_BYTES} byte safety limit");
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
            if let Some(run) = &job.run {
                println!(
                    "{}: '{}' ({}) [run={} started={} pid={}]",
                    job.name,
                    job.prompt,
                    job.expression,
                    run.id,
                    run.started_at,
                    run.pid
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "unbound".into())
                );
            } else {
                println!("{}: '{}' ({})", job.name, job.prompt, job.expression);
            }
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
    let name = name.unwrap_or_else(|| format!("job-{}", Utc::now().timestamp_millis()));
    let job = ScheduledJob {
        name: name.clone(),
        expression: expression.into(),
        prompt: prompt.into(),
        model,
        yolo,
        created_at: Some(Utc::now()),
        last_run: None,
        expires_at: None,
        run: None,
    };
    validate_job(&job)?;
    let saved_name = name.clone();
    with_jobs(move |jobs| {
        jobs.retain(|j| j.name != name);
        jobs.push(job);
        Ok(())
    })
    .await?;
    println!("scheduled job '{saved_name}'");
    Ok(())
}

pub async fn delete_job(name: &str) -> Result<()> {
    validate_job_name(name)?;
    with_jobs(|jobs| {
        let before = jobs.len();
        jobs.retain(|j| j.name != name);
        if jobs.len() == before {
            bail!("job '{name}' not found");
        }
        Ok(())
    })
    .await?;
    println!("deleted job '{name}'");
    Ok(())
}

fn job_generation(job: &ScheduledJob) -> Result<String> {
    let bytes = serde_json::to_vec(&(
        &job.name,
        &job.expression,
        &job.prompt,
        &job.model,
        job.yolo,
        job.created_at,
        job.expires_at,
    ))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn claim_process_is_live(claim: &JobRunClaim) -> Result<Option<bool>> {
    let (Some(pid), Some(expected_start)) = (claim.pid, claim.process_start) else {
        return Ok(None);
    };
    if !crate::process_alive(pid) {
        return Ok(Some(false));
    }
    Ok(Some(
        crate::lsp::process_start_identity(pid)? == expected_start,
    ))
}

fn claim_blocks_reentry(job: &ScheduledJob, now: DateTime<Utc>) -> Result<bool> {
    let Some(claim) = &job.run else {
        return Ok(false);
    };
    if job.yolo || claim.started_at > now {
        return Ok(true);
    }
    if claim_process_is_live(claim)?.is_some_and(|live| live) {
        return Ok(true);
    }
    Ok(now
        .signed_duration_since(claim.started_at)
        .to_std()
        .is_ok_and(|age| age < STALE_JOB_CLAIM_AFTER))
}

async fn claim_job_run(name: &str) -> Result<(ScheduledJob, String)> {
    claim_job_run_checked(name, None, false).await
}

async fn claim_job_run_checked(
    name: &str,
    expected_generation: Option<&str>,
    require_due: bool,
) -> Result<(ScheduledJob, String)> {
    validate_job_name(name)?;
    let _lock = lock_schedule().await?;
    let mut jobs = load_jobs()?;
    let idx = jobs
        .iter()
        .position(|job| job.name == name)
        .ok_or_else(|| anyhow::anyhow!("job '{name}' not found"))?;
    let now = Utc::now();
    if let Some(expected) = expected_generation
        && job_generation(&jobs[idx])? != expected
    {
        bail!("scheduled job '{name}' changed after it became due");
    }
    if require_due && !is_due(&jobs[idx]) {
        bail!("scheduled job '{name}' is no longer due");
    }
    if claim_blocks_reentry(&jobs[idx], now)? {
        if jobs[idx].yolo {
            bail!(
                "job '{name}' has an unresolved yolo run; inspect its effects and use `omgb schedule resolve-run {name} --confirm` before retrying"
            );
        }
        bail!("job '{name}' is already running or has an unexpired claim");
    }
    let run_id = uuid::Uuid::new_v4().to_string();
    jobs[idx].run = Some(JobRunClaim {
        id: run_id.clone(),
        started_at: now,
        pid: None,
        process_start: None,
    });
    jobs[idx].last_run = Some(now);
    save_jobs(&jobs)?;
    Ok((jobs[idx].clone(), run_id))
}

async fn release_job_run(name: &str, run_id: &str) -> Result<()> {
    let _lock = lock_schedule().await?;
    let mut jobs = load_jobs()?;
    let Some(job) = jobs.iter_mut().find(|job| job.name == name) else {
        return Ok(());
    };
    if job.run.as_ref().is_some_and(|claim| claim.id == run_id) {
        job.run = None;
        save_jobs(&jobs)?;
    }
    Ok(())
}

async fn bind_job_run_process(
    name: &str,
    run_id: &str,
    pid: u32,
    process_start: u64,
) -> Result<()> {
    let _lock = lock_schedule().await?;
    let mut jobs = load_jobs()?;
    let job = jobs
        .iter_mut()
        .find(|job| job.name == name)
        .ok_or_else(|| anyhow::anyhow!("job '{name}' disappeared before process binding"))?;
    let claim = job
        .run
        .as_mut()
        .filter(|claim| claim.id == run_id)
        .ok_or_else(|| anyhow::anyhow!("job '{name}' run claim changed before process binding"))?;
    claim.pid = Some(pid);
    claim.process_start = Some(process_start);
    save_jobs(&jobs)
}

pub async fn resolve_job_run(name: &str, confirm: bool) -> Result<()> {
    validate_job_name(name)?;
    if !confirm {
        bail!(
            "refusing to clear a scheduler run claim without --confirm after verifying its process and side effects"
        );
    }
    let _lock = lock_schedule().await?;
    let mut jobs = load_jobs()?;
    let job = jobs
        .iter_mut()
        .find(|job| job.name == name)
        .ok_or_else(|| anyhow::anyhow!("job '{name}' not found"))?;
    let claim = job
        .run
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("job '{name}' has no unresolved run"))?;
    if claim_process_is_live(claim)?.is_some_and(|live| live) {
        bail!(
            "job '{name}' is still owned by live pid {}; refusing to clear its run claim",
            claim.pid.unwrap_or_default()
        );
    }
    let run_id = claim.id.clone();
    job.run = None;
    save_jobs(&jobs)?;
    println!("resolved scheduler run {run_id} for '{name}'");
    Ok(())
}

pub async fn run_job(name: &str, capture: bool) -> Result<()> {
    run_job_cancellable(name, capture, None).await
}

async fn run_job_cancellable(
    name: &str,
    capture: bool,
    cancellation: Option<CancellationToken>,
) -> Result<()> {
    let (job, run_id) = claim_job_run(name).await?;
    let result = execute_job(&job, &run_id, capture, cancellation).await;
    let release = if result.is_err() && job.yolo {
        Ok(())
    } else {
        release_job_run(name, &run_id).await
    };
    match (result, release) {
        (Err(error), _) if job.yolo => Err(error).context(format!(
            "scheduler yolo run {run_id} remains ambiguous; inspect effects and resolve it explicitly"
        )),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("release scheduler job claim"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

async fn run_scheduled_job_cancellable(
    snapshot: ScheduledJob,
    cancellation: CancellationToken,
) -> Result<()> {
    let generation = job_generation(&snapshot)?;
    let (job, run_id) = claim_job_run_checked(&snapshot.name, Some(&generation), true).await?;
    let result = execute_job(&job, &run_id, true, Some(cancellation)).await;
    let release = if result.is_err() && job.yolo {
        Ok(())
    } else {
        release_job_run(&job.name, &run_id).await
    };
    match (result, release) {
        (Err(error), _) if job.yolo => Err(error).context(format!(
            "scheduler yolo run {run_id} remains ambiguous; inspect effects and resolve it explicitly"
        )),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("release scheduler job claim"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

async fn wait_for_scheduled_child(
    child: &mut tokio::process::Child,
    group: Option<&xai_tty_utils::ProcessGroup>,
    cancellation: Option<CancellationToken>,
) -> Result<std::process::ExitStatus> {
    let cancelled = async {
        match cancellation {
            Some(token) => token.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        status = tokio::time::timeout(DEFAULT_JOB_TIMEOUT, child.wait()) => {
            match status {
                Ok(status) => Ok(status?),
                Err(_) => {
                    crate::kill_child_and_reap(child, group).await;
                    bail!("scheduled job timed out after {}s", DEFAULT_JOB_TIMEOUT.as_secs());
                }
            }
        }
        _ = cancelled => {
            crate::kill_child_and_reap(child, group).await;
            bail!("scheduled job cancelled during daemon shutdown");
        }
    }
}

async fn execute_job(
    job: &ScheduledJob,
    run_id: &str,
    capture: bool,
    cancellation: Option<CancellationToken>,
) -> Result<()> {
    let name = &job.name;
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
    let process_identity = child
        .id()
        .context("scheduled job child exited before PID binding")
        .and_then(|pid| {
            crate::lsp::process_start_identity(pid)
                .context("bind scheduled job process start identity")
                .map(|process_start| (pid, process_start))
        });
    let (pid, process_start) = match process_identity {
        Ok(identity) => identity,
        Err(error) => {
            crate::kill_child_and_reap(&mut child, group.as_ref()).await;
            return Err(error);
        }
    };
    if let Err(error) = bind_job_run_process(name, run_id, pid, process_start).await {
        crate::kill_child_and_reap(&mut child, group.as_ref()).await;
        return Err(error.context("persist scheduled job process identity"));
    }
    if job.yolo && group.is_none() {
        crate::kill_child_and_reap(&mut child, None).await;
        bail!("could not establish scheduled-job process-tree containment");
    }

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
        let (failure_tx, mut failure_rx) = tokio::sync::mpsc::channel::<String>(3);
        let writer_failure_tx = failure_tx.clone();
        let writer = tokio::task::spawn_blocking(move || {
            let result = write_scheduler_log(&log_path, || rx.blocking_recv());
            if let Err(error) = &result {
                let _ = writer_failure_tx
                    .blocking_send(format!("scheduler log writer failed: {error}"));
            }
            result
        });
        let stdout_failure_tx = failure_tx.clone();
        let stdout_tx = tx.clone();
        let copy_out = tokio::spawn(async move {
            let result = copy_stream_to_log_sender(stdout, "stdout", stdout_tx).await;
            if let Err(error) = &result {
                let _ = stdout_failure_tx
                    .send(format!("scheduler stdout capture failed: {error}"))
                    .await;
            }
            result
        });
        let copy_err = tokio::spawn(async move {
            let result = copy_stream_to_log_sender(stderr, "stderr", tx).await;
            if let Err(error) = &result {
                let _ = failure_tx
                    .send(format!("scheduler stderr capture failed: {error}"))
                    .await;
            }
            result
        });
        enum CaptureOutcome {
            Child(Result<std::process::ExitStatus>),
            Failed(String),
        }
        let outcome = {
            let child_wait = wait_for_scheduled_child(&mut child, group.as_ref(), cancellation);
            tokio::pin!(child_wait);
            tokio::select! {
                biased;
                status = &mut child_wait => CaptureOutcome::Child(status),
                failure = failure_rx.recv() => CaptureOutcome::Failed(
                    failure.unwrap_or_else(|| "scheduler capture monitoring stopped".into())
                ),
            }
        };
        let status = match outcome {
            CaptureOutcome::Child(Ok(status)) => status,
            CaptureOutcome::Child(Err(error)) => {
                copy_out.abort();
                copy_err.abort();
                writer.abort();
                let _ = tokio::time::timeout(Duration::from_secs(5), async {
                    tokio::join!(copy_out, copy_err, writer)
                })
                .await;
                return Err(error).with_context(|| format!("job '{name}' did not complete"));
            }
            CaptureOutcome::Failed(error) => {
                crate::kill_child_and_reap(&mut child, group.as_ref()).await;
                copy_out.abort();
                copy_err.abort();
                writer.abort();
                let _ = tokio::time::timeout(Duration::from_secs(5), async {
                    tokio::join!(copy_out, copy_err, writer)
                })
                .await;
                bail!("{error}; job '{name}' process tree was terminated");
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
        let status = wait_for_scheduled_child(&mut child, group.as_ref(), cancellation)
            .await
            .with_context(|| format!("job '{name}' did not complete"))?;
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
        if let Ok(record) = read_scheduler_pid(&path)
            && scheduler_process_matches(&record).unwrap_or(false)
            && scheduler_pid_lock_held(&path).unwrap_or(false)
        {
            println!("scheduler daemon started (pid {})", record.pid);
            return Ok(());
        }
    }
    bail!("scheduler daemon failed to start")
}

async fn shutdown_request_signal() -> Result<()> {
    let path = stop_request_path()?;
    loop {
        if path.is_file() {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
            result = shutdown_request_signal() => result?,
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            result = shutdown_request_signal() => result?,
        }
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
            signal = shutdown_signal() => {
                signal?;
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
        let mut remaining = due_jobs.into_iter();
        let mut set = tokio::task::JoinSet::new();
        let cancellation = CancellationToken::new();
        for job in remaining.by_ref().take(MAX_CONCURRENT_SCHEDULE_JOBS) {
            let cancellation = cancellation.clone();
            set.spawn(async move { run_scheduled_job_cancellable(job, cancellation).await });
        }
        let shutdown = shutdown_signal();
        tokio::pin!(shutdown);
        while !set.is_empty() {
            tokio::select! {
                res = set.join_next() => {
                    if let Some(res) = res {
                        match res {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => eprintln!("scheduler: job error: {e}"),
                            Err(e) => eprintln!("scheduler: job task panicked: {e}"),
                        }
                    }
                    if let Some(job) = remaining.next() {
                        let cancellation = cancellation.clone();
                        set.spawn(async move {
                            run_scheduled_job_cancellable(job, cancellation).await
                        });
                    }
                }
                signal = &mut shutdown => {
                    println!("scheduler: cancelling active jobs for shutdown");
                    cancellation.cancel();
                    signal?;
                    while let Some(result) = set.join_next().await {
                        if let Err(error) = result {
                            eprintln!("scheduler: shutdown task error: {error}");
                        }
                    }
                    return Ok(());
                }
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
    validate_job_name(id)?;
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
    if !path.exists() {
        println!("scheduler is not running");
        return Ok(());
    }
    if !scheduler_pid_lock_held(&path)? {
        let _ = std::fs::remove_file(stop_request_path()?);
        println!("scheduler is not running");
        return Ok(());
    }
    let record = read_scheduler_pid(&path)?;
    if !scheduler_process_matches(&record)? {
        bail!(
            "scheduler PID file is locked, but its recorded process identity does not match; refusing to signal pid {}",
            record.pid
        );
    }
    crate::providers::write_file_atomic(&stop_request_path()?, b"stop\n", true)?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if !scheduler_pid_lock_held(&path).unwrap_or(false) {
            let _ = std::fs::remove_file(stop_request_path()?);
            println!("stopped scheduler (pid {})", record.pid);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !scheduler_pid_lock_held(&path)? || !scheduler_process_matches(&record)? {
        bail!(
            "scheduler ownership changed while stopping; refusing to force-kill pid {}",
            record.pid
        );
    }
    eprintln!(
        "warning: scheduler did not stop gracefully after 10 seconds; forcing pid {}",
        record.pid
    );
    #[cfg(unix)]
    let status = {
        let kill = which::which("kill").unwrap_or_else(|_| PathBuf::from("/bin/kill"));
        std::process::Command::new(kill)
            .args(["-KILL", &record.pid.to_string()])
            .status()?
    };
    #[cfg(not(unix))]
    let status = {
        let taskkill = which::which("taskkill")
            .unwrap_or_else(|_| PathBuf::from(r"C:\Windows\System32\taskkill.exe"));
        std::process::Command::new(taskkill)
            .args(["/PID", &record.pid.to_string(), "/T", "/F"])
            .status()?
    };
    if !status.success() {
        bail!("failed to force-stop scheduler (pid {})", record.pid);
    }
    let forced = Instant::now();
    while forced.elapsed() < Duration::from_secs(5) {
        if !scheduler_process_matches(&record).unwrap_or(false) {
            let _ = std::fs::remove_file(stop_request_path()?);
            println!("force-stopped scheduler (pid {})", record.pid);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!(
        "scheduler (pid {}) did not exit after force-stop",
        record.pid
    )
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
    let now = Utc::now();
    if is_expired(job, now) {
        return false;
    }
    if claim_blocks_reentry(job, now).unwrap_or(true) {
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
    } else if let Some(created_at) = job.created_at {
        match cron.find_next_occurrence(&created_at.with_timezone(&Local), false) {
            Ok(next) => now >= next,
            Err(_) => false,
        }
    } else {
        // Legacy records did not persist creation time. Preserve their former
        // one-time immediate behavior; the resulting run records last_run.
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
    if !total.is_finite() || total > i64::MAX as f64 {
        return None;
    }
    let secs = total as u64;
    if secs == 0 || secs > i64::MAX as u64 {
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

fn write_scheduler_log(path: &Path, next_chunk: impl FnMut() -> Option<Vec<u8>>) -> Result<()> {
    write_scheduler_log_with_limit(path, MAX_SCHEDULER_LOG_BYTES, next_chunk)
}

fn write_scheduler_log_with_limit(
    path: &Path,
    max_bytes: u64,
    mut next_chunk: impl FnMut() -> Option<Vec<u8>>,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("scheduler log path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
    {
        Ok(file) => drop(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("create scheduler log"),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        bail!("scheduler log is not a regular file: {}", path.display());
    }
    // Set the ACL before reopening the file; Windows rejects ACL replacement
    // on some handles opened for append.
    crate::providers::restrict_omg_file_permissions(path)
        .context("restrict scheduler log permissions")?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .context("open scheduler log for append")?;

    let mut rotate_if_full = true;
    while let Some(chunk) = next_chunk() {
        file.lock_exclusive()
            .context("lock scheduler log for append")?;
        let result = (|| -> Result<()> {
            let mut size = file.metadata().context("inspect scheduler log")?.len();
            if rotate_if_full && size >= max_bytes {
                file.set_len(0).context("truncate full scheduler log")?;
                size = 0;
            }
            rotate_if_full = false;
            let remaining = max_bytes.saturating_sub(size) as usize;
            if remaining > 0 {
                file.seek(SeekFrom::End(0))
                    .context("seek to scheduler log end")?;
                file.write_all(&chunk[..chunk.len().min(remaining)])
                    .context("append scheduler log chunk")?;
            }
            Ok(())
        })();
        let unlock = file.unlock();
        result?;
        unlock.context("unlock scheduler log")?;
    }
    file.sync_data().context("sync scheduler log")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeDelta, TimeZone};

    fn tmp_home() -> PathBuf {
        std::env::temp_dir().join(format!("omgb-scheduler-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn test_parse_interval() {
        assert_eq!(parse_interval("30s"), Some(30));
        assert_eq!(parse_interval("5m"), Some(300));
        assert_eq!(parse_interval("2h"), Some(7200));
        assert_eq!(parse_interval("1d"), Some(86400));
        assert_eq!(parse_interval("1h30m"), Some(5400));
        assert_eq!(parse_interval(" 90 M "), Some(5400));
        assert_eq!(parse_interval("foo"), None);
        assert_eq!(parse_interval("999999999999999999999999s"), None);
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
            created_at: None,
            last_run: Some(Utc::now() - TimeDelta::seconds(90)),
            expires_at: None,
            run: None,
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
            created_at: None,
            last_run: None,
            expires_at: None,
            run: None,
        };
        assert!(is_due(&job));
    }

    #[test]
    fn newly_created_cron_waits_for_its_first_occurrence() {
        let job = ScheduledJob {
            name: "future".into(),
            expression: "* * * * *".into(),
            prompt: "do work".into(),
            model: None,
            yolo: false,
            created_at: Some(Utc::now()),
            last_run: None,
            expires_at: None,
            run: None,
        };
        assert!(!is_due(&job));
    }

    #[test]
    fn scheduler_job_claim_prevents_overlapping_runs() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        save_jobs(&[ScheduledJob {
            name: "claimed".into(),
            expression: "1m".into(),
            prompt: "do work".into(),
            model: None,
            yolo: false,
            created_at: None,
            last_run: None,
            expires_at: None,
            run: None,
        }])
        .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (claimed, run_id) = claim_job_run("claimed").await.unwrap();
            assert!(!is_due(&claimed));
            assert!(claim_job_run("claimed").await.is_err());
            release_job_run("claimed", &run_id).await.unwrap();
            assert!(claim_job_run("claimed").await.is_ok());
        });
        crate::providers::set_omg_home_for_tests(None);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn scheduler_claims_fail_closed_for_yolo_future_and_live_runs() {
        let now = Utc::now();
        let mut job = ScheduledJob {
            name: "claimed".into(),
            expression: "1m".into(),
            prompt: "do work".into(),
            model: None,
            yolo: false,
            created_at: None,
            last_run: None,
            expires_at: None,
            run: Some(JobRunClaim {
                id: uuid::Uuid::new_v4().to_string(),
                started_at: now + TimeDelta::minutes(1),
                pid: None,
                process_start: None,
            }),
        };
        assert!(claim_blocks_reentry(&job, now).unwrap());

        job.run.as_mut().unwrap().started_at = now - TimeDelta::hours(1);
        assert!(!claim_blocks_reentry(&job, now).unwrap());
        job.yolo = true;
        assert!(claim_blocks_reentry(&job, now).unwrap());

        job.yolo = false;
        let pid = std::process::id();
        job.run.as_mut().unwrap().pid = Some(pid);
        job.run.as_mut().unwrap().process_start =
            Some(crate::lsp::process_start_identity(pid).unwrap());
        assert!(claim_blocks_reentry(&job, now).unwrap());
    }

    #[test]
    fn daemon_claim_rejects_a_replaced_due_snapshot() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let due = ScheduledJob {
            name: "replace-me".into(),
            expression: "1m".into(),
            prompt: "old prompt".into(),
            model: None,
            yolo: false,
            created_at: None,
            last_run: None,
            expires_at: None,
            run: None,
        };
        let generation = job_generation(&due).unwrap();
        save_jobs(std::slice::from_ref(&due)).unwrap();
        let mut replacement = due;
        replacement.prompt = "new prompt".into();
        replacement.created_at = Some(Utc::now());
        replacement.expression = "0 0 1 1 *".into();
        save_jobs(&[replacement]).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(claim_job_run_checked("replace-me", Some(&generation), true))
            .unwrap_err();
        assert!(error.to_string().contains("changed after it became due"));
        crate::providers::set_omg_home_for_tests(None);
        std::fs::remove_dir_all(home).ok();
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
            created_at: None,
            last_run: None,
            expires_at: None,
            run: None,
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

    #[test]
    fn scheduler_pid_records_require_an_executable_identity() {
        let record =
            parse_scheduler_pid(r#"{"pid":42,"executable":"/usr/bin/omgb","process_start":12345}"#)
                .unwrap();
        assert_eq!(record.pid, 42);
        assert_eq!(record.executable, "/usr/bin/omgb");
        assert_eq!(record.process_start, 12345);
        assert!(parse_scheduler_pid(r#"{"pid":42,"executable":"/usr/bin/omgb"}"#).is_err());
        assert!(parse_scheduler_pid("42\n").is_err());
    }

    #[test]
    fn scheduler_pid_lock_proves_live_daemon_ownership() {
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        let path = home.join("scheduler.pid");
        let owner = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        assert!(!scheduler_pid_lock_held(&path).unwrap());
        owner.lock_exclusive().unwrap();
        assert!(scheduler_pid_lock_held(&path).unwrap());
        FileExt::unlock(&owner).unwrap();
        assert!(!scheduler_pid_lock_held(&path).unwrap());

        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn malformed_schedule_fails_closed_without_echoing_prompt_data() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let path = home.join("schedule.jsonl");
        let valid = serde_json::to_string(&ScheduledJob {
            name: "valid".into(),
            expression: "5m".into(),
            prompt: "keep".into(),
            model: None,
            yolo: true,
            created_at: None,
            last_run: None,
            expires_at: None,
            run: None,
        })
        .unwrap();
        std::fs::write(&path, format!("{valid}\n{{SUPERSECRET}}\n")).unwrap();

        let error = load_jobs().unwrap_err().to_string();
        assert!(error.contains("line 2"), "unexpected error: {error}");
        assert!(!error.contains("SUPERSECRET"));

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn scheduler_log_is_capped_and_rotates_on_the_next_run() {
        let home = tmp_home();
        let path = home.join("scheduler.log");
        let mut first = vec![b"abcdef".to_vec(), b"ghijkl".to_vec()].into_iter();
        write_scheduler_log_with_limit(&path, 10, || first.next()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdefghij");

        let mut second = vec![b"xy".to_vec()].into_iter();
        write_scheduler_log_with_limit(&path, 10, || second.next()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"xy");

        std::fs::remove_dir_all(&home).ok();
    }
}
