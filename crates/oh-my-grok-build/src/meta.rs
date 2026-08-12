//! Meta-harness for `omgb`.
//!
//! Plans high-level goals into subtasks, spawns persistent threads, tracks
//! execution, and emits notifications so the harness can observe and evolve.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::args::{MetaArgs, MetaCommand, MetaNotificationsArgs, MetaResolution, MetaRunArgs};

const PLAN_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SubtaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PlanStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    pub id: String,
    pub description: String,
    pub thread_id: Option<String>,
    pub model: Option<String>,
    pub status: SubtaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_thread_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaPlan {
    pub id: String,
    pub goal: String,
    pub created_at: DateTime<Utc>,
    pub status: PlanStatus,
    pub yolo: bool,
    pub subtasks: Vec<Subtask>,
}

fn meta_dir() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("meta"))
}

fn plans_dir() -> Result<PathBuf> {
    Ok(meta_dir()?.join("plans"))
}

fn plan_path(id: &str) -> Result<PathBuf> {
    crate::threads::validate_id(id)?;
    Ok(plans_dir()?.join(format!("{id}.json")))
}

fn plan_lock_path(id: &str) -> Result<PathBuf> {
    crate::threads::validate_id(id)?;
    Ok(meta_dir()?.join("locks").join(format!("{id}.lock")))
}

async fn acquire_plan_lock(id: &str) -> Result<std::fs::File> {
    let path = plan_lock_path(id)?;
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        let deadline = std::time::Instant::now() + PLAN_LOCK_TIMEOUT;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => break,
                Err(error)
                    if error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error)
                    if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
                {
                    bail!("timed out waiting for plan ownership; another run may still be active");
                }
                Err(error) => return Err(error.into()),
            }
        }
        crate::providers::restrict_omg_file_permissions(&path)?;
        Ok(file)
    })
    .await
    .context("meta plan lock task failed")?
}

fn save_plan(plan: &MetaPlan) -> Result<()> {
    let path = plan_path(&plan.id)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("plans path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    crate::providers::write_file_atomic(&path, serde_json::to_string_pretty(plan)?, true)
        .with_context(|| format!("write {}", path.display()))
}

fn persist_plan_error(plan: &MetaPlan, error: anyhow::Error, context: &str) -> anyhow::Error {
    match save_plan(plan) {
        Ok(()) => error,
        Err(save_error) => anyhow::anyhow!("{error}; {context}: {save_error}"),
    }
}

fn load_plan(id: &str) -> Result<MetaPlan> {
    let path = plan_path(id)?;
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{e}"))
}

fn list_plans() -> Result<Vec<MetaPlan>> {
    let dir = plans_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut plans = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if let (Some(ext), Ok(raw)) = (path.extension(), std::fs::read_to_string(&path))
            && ext == "json"
            && let Ok(plan) = serde_json::from_str::<MetaPlan>(&raw)
        {
            plans.push(plan);
        }
    }
    Ok(plans)
}

pub async fn run_meta(args: MetaArgs) -> Result<()> {
    match args.command {
        MetaCommand::Run(args) => run_run(args).await,
        MetaCommand::List => list(),
        MetaCommand::Show { id } => show(&id),
        MetaCommand::Resume { id } => resume(&id).await,
        MetaCommand::Resolve {
            id,
            subtask,
            action,
            result,
            confirm,
        } => resolve(&id, &subtask, action, result, confirm).await,
        MetaCommand::Notifications(args) => notifications(args),
    }
}

async fn run_run(args: MetaRunArgs) -> Result<()> {
    let mut plan = build_plan(&args.goal, args.model, args.yolo).await?;
    let _plan_lock = acquire_plan_lock(&plan.id).await?;
    plan.status = PlanStatus::Running;
    save_plan(&plan)?;
    if let Err(e) = execute_plan(&mut plan).await {
        plan.status = PlanStatus::Failed;
        return Err(persist_plan_error(
            &plan,
            e,
            "failed to persist the failed plan state",
        ));
    }
    plan.status = PlanStatus::Completed;
    save_plan(&plan)?;
    if let Err(error) = crate::notifications::push(
        "plan_completed",
        serde_json::json!({"plan_id": plan.id, "goal": plan.goal}),
    ) {
        eprintln!("warning: plan completed but notification could not be recorded: {error}");
    }
    println!(
        "plan {} completed ({} subtasks)",
        plan.id,
        plan.subtasks.len()
    );
    Ok(())
}

async fn build_plan(goal: &str, model: Option<String>, yolo: bool) -> Result<MetaPlan> {
    let descriptions = plan_subtasks(goal, model.clone()).await?;
    let subtasks = descriptions
        .into_iter()
        .enumerate()
        .map(|(i, d)| Subtask {
            id: format!("t{}", i + 1),
            description: d,
            thread_id: None,
            model: model.clone(),
            status: SubtaskStatus::Pending,
            result: None,
            previous_thread_ids: Vec::new(),
        })
        .collect();
    Ok(MetaPlan {
        id: uuid::Uuid::new_v4().to_string(),
        goal: goal.to_string(),
        created_at: Utc::now(),
        status: PlanStatus::Pending,
        yolo,
        subtasks,
    })
}

async fn execute_plan(plan: &mut MetaPlan) -> Result<()> {
    for i in 0..plan.subtasks.len() {
        match plan.subtasks[i].status {
            SubtaskStatus::Completed => continue,
            SubtaskStatus::Failed => {
                bail!(
                    "subtask {} previously failed and cannot be resumed automatically",
                    plan.subtasks[i].id
                );
            }
            SubtaskStatus::Ambiguous => {
                bail!(
                    "subtask {} has an ambiguous prior attempt; inspect its thread and use `omgb meta resolve {} {} complete|retry --confirm` before resuming",
                    plan.subtasks[i].id,
                    plan.id,
                    plan.subtasks[i].id
                );
            }
            SubtaskStatus::Pending | SubtaskStatus::Running => {}
        }
        if matches!(plan.subtasks[i].status, SubtaskStatus::Pending) {
            let subtask = &mut plan.subtasks[i];
            subtask.thread_id = Some(uuid::Uuid::new_v4().to_string());
            subtask.status = SubtaskStatus::Running;
            subtask.result = None;
            save_plan(plan)?;
        }
        let desc = plan.subtasks[i].description.clone();
        let model = plan.subtasks[i].model.clone();
        let yolo = plan.yolo;
        let thread_id = plan.subtasks[i]
            .thread_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("running subtask has no durable thread id"))?;
        let existing_model = crate::threads::thread_model(&thread_id)?;
        let execution = if let Some(used_model) = existing_model {
            match crate::threads::initial_assistant_text(&thread_id).await {
                Ok(output) => Ok((used_model, output)),
                Err(error) => Err(error.context(format!(
                    "thread '{thread_id}' exists but has no reconcilable assistant result"
                ))),
            }
        } else {
            match crate::threads::create(&desc, model.clone(), yolo, Some(thread_id.clone())).await
            {
                Ok((_, used_model)) => crate::threads::initial_assistant_text(&thread_id)
                    .await
                    .map(|output| (used_model, output)),
                Err(error) => Err(error),
            }
        };
        match execution {
            Ok((used_model, output)) => {
                let verification =
                    verify_subtask(&desc, &output, Some(used_model.clone()), yolo).await;
                let mut incomplete = None;
                {
                    let subtask = &mut plan.subtasks[i];
                    subtask.model = Some(used_model);
                    match verification {
                        Ok((completed, summary)) => {
                            subtask.status = if completed {
                                SubtaskStatus::Completed
                            } else {
                                SubtaskStatus::Failed
                            };
                            subtask.result = Some(summary.clone());
                            if !completed {
                                incomplete = Some((subtask.id.clone(), summary));
                            }
                        }
                        Err(e) => {
                            subtask.status = SubtaskStatus::Failed;
                            subtask.result = Some(format!("verification failed: {e}"));
                            return Err(persist_plan_error(
                                plan,
                                e,
                                "failed to persist the verification failure",
                            ));
                        }
                    }
                }
                if let Some((id, summary)) = incomplete {
                    let error = anyhow::anyhow!("subtask {id} not completed: {summary}");
                    return Err(persist_plan_error(
                        plan,
                        error,
                        "failed to persist the incomplete subtask state",
                    ));
                }
                save_plan(plan)?;
            }
            Err(e) => {
                let attempt_exists = crate::threads::thread_model(&thread_id)?.is_some();
                {
                    let subtask = &mut plan.subtasks[i];
                    subtask.status = if attempt_exists {
                        SubtaskStatus::Ambiguous
                    } else {
                        SubtaskStatus::Failed
                    };
                    subtask.result = Some(if attempt_exists {
                        format!("thread attempt may have executed and requires reconciliation: {e}")
                    } else {
                        format!("thread creation failed before a thread was published: {e}")
                    });
                }
                return Err(persist_plan_error(
                    plan,
                    e,
                    "failed to persist the ambiguous subtask state",
                ));
            }
        }
    }
    Ok(())
}

async fn resolve(
    id: &str,
    subtask_id: &str,
    action: MetaResolution,
    result: Option<String>,
    confirm: bool,
) -> Result<()> {
    if !confirm {
        bail!("refusing to resolve ambiguous meta work without --confirm");
    }
    let _plan_lock = acquire_plan_lock(id).await?;
    let mut plan = load_plan(id)?;
    let subtask = plan
        .subtasks
        .iter_mut()
        .find(|subtask| subtask.id == subtask_id)
        .ok_or_else(|| anyhow::anyhow!("subtask '{subtask_id}' not found in plan '{id}'"))?;
    if !matches!(subtask.status, SubtaskStatus::Ambiguous) {
        bail!("subtask '{subtask_id}' is not in an ambiguous state");
    }
    match action {
        MetaResolution::Complete => {
            subtask.status = SubtaskStatus::Completed;
            subtask.result =
                Some(result.unwrap_or_else(|| "manually reconciled as complete".into()));
        }
        MetaResolution::Retry => {
            if let Some(previous) = subtask.thread_id.take() {
                subtask.previous_thread_ids.push(previous);
            }
            subtask.status = SubtaskStatus::Pending;
            subtask.result = Some("operator confirmed a new attempt is safe".into());
        }
    }
    plan.status = PlanStatus::Pending;
    save_plan(&plan)?;
    println!("resolved plan {id} subtask {subtask_id}; run `omgb meta resume {id}` to continue");
    Ok(())
}

async fn verify_subtask(
    description: &str,
    output: &str,
    model: Option<String>,
    yolo: bool,
) -> Result<(bool, String)> {
    let prompt = format!(
        "You are checking whether a subtask was completed.\n\n\
         Subtask: {description}\n\n\
         Worker output:\n{output}\n\n\
         If the subtask is completed, start your reply with:\n\
         COMPLETED: <one-sentence summary>\n\n\
         If the subtask is not completed, start your reply with:\n\
         INCOMPLETE: <reason>"
    );
    let verdict =
        crate::swarm::exec_plain(&prompt, model, yolo, Some("read_file,grep,list_dir")).await?;
    let first = verdict.lines().next().unwrap_or("").trim();
    if let Some(summary) = first.strip_prefix("COMPLETED:") {
        return Ok((true, summary.trim().to_string()));
    }
    if let Some(reason) = first.strip_prefix("INCOMPLETE:") {
        return Ok((false, reason.trim().to_string()));
    }
    if output.trim().is_empty() {
        return Ok((false, "worker produced no output".into()));
    }
    Ok((false, format!("verifier did not follow format: {first}")))
}

async fn resume(id: &str) -> Result<()> {
    let _plan_lock = acquire_plan_lock(id).await?;
    let mut plan = load_plan(id)?;
    plan.status = PlanStatus::Running;
    save_plan(&plan)?;
    if let Err(e) = execute_plan(&mut plan).await {
        plan.status = PlanStatus::Failed;
        return Err(persist_plan_error(
            &plan,
            e,
            "failed to persist the failed resumed plan state",
        ));
    }
    plan.status = PlanStatus::Completed;
    save_plan(&plan)?;
    if let Err(error) =
        crate::notifications::push("plan_resumed", serde_json::json!({"plan_id": plan.id}))
    {
        eprintln!("warning: plan resumed but notification could not be recorded: {error}");
    }
    println!("plan {} resumed and completed", plan.id);
    Ok(())
}

fn list() -> Result<()> {
    let mut plans = list_plans()?;
    if plans.is_empty() {
        println!("no plans");
        return Ok(());
    }
    plans.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    for p in plans {
        println!(
            "{} [{:?}] {} ({} subtasks)",
            p.id,
            p.status,
            p.goal.lines().next().unwrap_or(""),
            p.subtasks.len()
        );
    }
    Ok(())
}

fn show(id: &str) -> Result<()> {
    let plan = load_plan(id)?;
    println!("plan {} [{:?}]", plan.id, plan.status);
    println!("goal: {}", plan.goal);
    for s in &plan.subtasks {
        let thread = s.thread_id.as_deref().unwrap_or("-");
        let result = s.result.as_deref().unwrap_or("-");
        let previous = if s.previous_thread_ids.is_empty() {
            "-".to_string()
        } else {
            s.previous_thread_ids.join(",")
        };
        println!(
            "  {} [{:?}] thread={} previous_threads={} result={} {}",
            s.id, s.status, thread, previous, result, s.description
        );
    }
    Ok(())
}

fn notifications(args: MetaNotificationsArgs) -> Result<()> {
    let notifs = crate::notifications::list(args.limit)?;
    if notifs.is_empty() {
        println!("no notifications");
        return Ok(());
    }
    for n in notifs {
        println!(
            "{} {}\n  {}",
            n.timestamp.format("%Y-%m-%d %H:%M UTC"),
            n.event_type,
            serde_json::to_string(&n.data).unwrap_or_default()
        );
    }
    Ok(())
}

async fn plan_subtasks(goal: &str, model: Option<String>) -> Result<Vec<String>> {
    const MAX_PLANNER_OUTPUT_BYTES: usize = 256 * 1024;
    const PLANNER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    let prompt = format!(
        "Given the goal below, produce a JSON array of concise, self-contained subtask strings \
         that an agent thread can execute independently. Return ONLY a JSON array of strings, \
         with no markdown, no explanation, no code fences.\n\nExample: [\"subtask 1\", \"subtask 2\"]\n\nGoal: {goal}"
    );
    let prompt_file = crate::write_prompt_temp(&prompt).await?;
    let _guard = crate::PromptFileGuard(prompt_file.clone());
    let mut cmd = tokio::process::Command::new(std::env::current_exe()?);
    cmd.arg("exec")
        .arg("--prompt-file")
        .arg(&prompt_file)
        .arg("--json")
        .arg("--disallowed-tools")
        .arg(crate::all_tool_ids_csv().as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(m) = &model {
        cmd.arg("--model").arg(m);
    }
    let (mut child, group) = crate::spawn_with_process_group(cmd)?;
    let mut stdout = child.stdout.take().context("planner stdout not piped")?;
    let capture = tokio::spawn(async move {
        let mut capture = crate::BoundedCapture::new(MAX_PLANNER_OUTPUT_BYTES + 1);
        tokio::io::copy(&mut stdout, &mut capture).await?;
        Ok::<_, std::io::Error>(capture.into_string())
    });
    let status = match tokio::time::timeout(PLANNER_TIMEOUT, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            crate::kill_child_and_reap(&mut child, group.as_ref()).await;
            capture.abort();
            bail!(
                "plan generation timed out after {}s",
                PLANNER_TIMEOUT.as_secs()
            );
        }
    };
    crate::kill_process_group(group.as_ref());
    let text = capture.await.context("planner output task failed")??;
    if !status.success() {
        bail!("plan generation failed");
    }
    if text.len() > MAX_PLANNER_OUTPUT_BYTES {
        bail!("plan output exceeds the {MAX_PLANNER_OUTPUT_BYTES} byte limit");
    }
    crate::swarm::parse_subtasks(&text).context("failed to parse plan subtasks")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("omgb-meta-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn test_plan_path_validation() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));

        assert!(plan_path("abc-123").is_ok());
        assert!(plan_path("").is_err());
        assert!(plan_path("../x").is_err());
        assert!(plan_path("a b").is_err());

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn test_save_load_and_list_plans() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));

        let plan = MetaPlan {
            id: "abc".into(),
            goal: "test goal".into(),
            created_at: Utc::now(),
            status: PlanStatus::Pending,
            yolo: false,
            subtasks: vec![Subtask {
                id: "t1".into(),
                description: "do it".into(),
                thread_id: None,
                model: None,
                status: SubtaskStatus::Pending,
                result: None,
                previous_thread_ids: Vec::new(),
            }],
        };
        save_plan(&plan).unwrap();
        let loaded = load_plan("abc").unwrap();
        assert_eq!(loaded.goal, "test goal");
        assert_eq!(loaded.subtasks.len(), 1);

        let plans = list_plans().unwrap();
        assert_eq!(plans.len(), 1);

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn ambiguous_subtask_retry_preserves_the_prior_thread_as_evidence() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let plan = MetaPlan {
            id: "recover-plan".into(),
            goal: "recover safely".into(),
            created_at: Utc::now(),
            status: PlanStatus::Failed,
            yolo: true,
            subtasks: vec![Subtask {
                id: "t1".into(),
                description: "perform a change".into(),
                thread_id: Some("prior-thread".into()),
                model: Some("omgb-test".into()),
                status: SubtaskStatus::Ambiguous,
                result: Some("connection lost".into()),
                previous_thread_ids: Vec::new(),
            }],
        };
        save_plan(&plan).unwrap();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(resolve(
                "recover-plan",
                "t1",
                MetaResolution::Retry,
                None,
                true,
            ))
            .unwrap();
        let recovered = load_plan("recover-plan").unwrap();
        let subtask = &recovered.subtasks[0];
        assert!(matches!(subtask.status, SubtaskStatus::Pending));
        assert!(subtask.thread_id.is_none());
        assert_eq!(subtask.previous_thread_ids, ["prior-thread"]);

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }
}
