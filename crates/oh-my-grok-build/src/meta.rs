//! Meta-harness for `omgb`.
//!
//! Plans high-level goals into subtasks, spawns persistent threads, tracks
//! execution, and emits notifications so the harness can observe and evolve.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::args::{MetaArgs, MetaCommand, MetaNotificationsArgs, MetaRunArgs};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SubtaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

fn save_plan(plan: &MetaPlan) -> Result<()> {
    let path = plan_path(&plan.id)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("plans path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    crate::providers::write_file_atomic(&path, serde_json::to_string_pretty(plan)?, true)
        .with_context(|| format!("write {}", path.display()))
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
        MetaCommand::Notifications(args) => notifications(args),
    }
}

async fn run_run(args: MetaRunArgs) -> Result<()> {
    let mut plan = build_plan(&args.goal, args.model, args.yolo).await?;
    plan.status = PlanStatus::Running;
    save_plan(&plan)?;
    if let Err(e) = execute_plan(&mut plan).await {
        plan.status = PlanStatus::Failed;
        let _ = save_plan(&plan);
        return Err(e);
    }
    plan.status = PlanStatus::Completed;
    save_plan(&plan)?;
    crate::notifications::push(
        "plan_completed",
        serde_json::json!({"plan_id": plan.id, "goal": plan.goal}),
    )?;
    println!(
        "plan {} completed ({} subtasks)",
        plan.id,
        plan.subtasks.len()
    );
    Ok(())
}

async fn build_plan(goal: &str, model: Option<String>, yolo: bool) -> Result<MetaPlan> {
    let descriptions = plan_subtasks(goal, model.clone(), yolo).await?;
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
        if matches!(
            plan.subtasks[i].status,
            SubtaskStatus::Completed | SubtaskStatus::Failed
        ) {
            continue;
        }
        {
            let subtask = &mut plan.subtasks[i];
            subtask.status = SubtaskStatus::Running;
        }
        save_plan(plan)?;
        let desc = plan.subtasks[i].description.clone();
        let model = plan.subtasks[i].model.clone();
        let yolo = plan.yolo;
        match crate::threads::create(&desc, model.clone(), yolo, None).await {
            Ok((thread_id, used_model)) => {
                let verification = match crate::threads::last_assistant_text(&thread_id).await {
                    Ok(output) => {
                        verify_subtask(&desc, &output, Some(used_model.clone()), yolo).await
                    }
                    Err(e) => Err(e),
                };
                let mut incomplete = None;
                {
                    let subtask = &mut plan.subtasks[i];
                    subtask.thread_id = Some(thread_id);
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
                            let _ = save_plan(plan);
                            return Err(e);
                        }
                    }
                }
                if let Some((id, summary)) = incomplete {
                    let _ = save_plan(plan);
                    bail!("subtask {} not completed: {}", id, summary);
                }
                save_plan(plan)?;
            }
            Err(e) => {
                {
                    let subtask = &mut plan.subtasks[i];
                    subtask.status = SubtaskStatus::Failed;
                    subtask.result = Some(format!("thread creation failed: {e}"));
                }
                let _ = save_plan(plan);
                return Err(e);
            }
        }
    }
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
    let mut plan = load_plan(id)?;
    plan.status = PlanStatus::Running;
    save_plan(&plan)?;
    if let Err(e) = execute_plan(&mut plan).await {
        plan.status = PlanStatus::Failed;
        let _ = save_plan(&plan);
        return Err(e);
    }
    plan.status = PlanStatus::Completed;
    save_plan(&plan)?;
    crate::notifications::push("plan_resumed", serde_json::json!({"plan_id": plan.id}))?;
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
        println!(
            "  {} [{:?}] thread={} result={} {}",
            s.id, s.status, thread, result, s.description
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

async fn plan_subtasks(goal: &str, model: Option<String>, yolo: bool) -> Result<Vec<String>> {
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
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(m) = &model {
        cmd.arg("--model").arg(m);
    }
    if yolo {
        cmd.arg("--yolo");
    }
    let out = cmd.output().await?;
    if !out.status.success() {
        bail!("plan generation failed");
    }
    let text = String::from_utf8_lossy(&out.stdout);
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
}
