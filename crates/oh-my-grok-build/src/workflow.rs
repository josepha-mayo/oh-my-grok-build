use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::args::{WorkflowArgs, WorkflowCommand, WorkflowNewArgs, WorkflowRunArgs};
use crate::{SessionParams, run_single_turn_with};
use xai_grok_pager::headless::OutputFormat;

#[derive(Debug, Serialize, Deserialize)]
struct Workflow {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    step: Vec<WorkflowStep>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WorkflowStep {
    Exec(ExecStep),
    FanOut(FanOutStep),
    Shell(ShellStep),
}

#[derive(Debug, Serialize, Deserialize)]
struct ExecStep {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    yolo: Option<bool>,
    #[serde(default)]
    tools: Option<String>,
    #[serde(default)]
    max_turns: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FanOutStep {
    prompt: String,
    count: usize,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    yolo: Option<bool>,
    #[serde(default)]
    tools: Option<String>,
    #[serde(default)]
    max_turns: Option<u32>,
    #[serde(default)]
    aggregate: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ShellStep {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    expect_exit: Option<i32>,
}

fn workflows_dir() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("workflows"))
}

fn resolve_workflow_path(name: &str) -> Result<PathBuf> {
    let dir = workflows_dir()?;
    let json = dir.join(format!("{name}.json"));
    if json.exists() {
        return Ok(json);
    }
    let toml = dir.join(format!("{name}.toml"));
    if toml.exists() {
        return Ok(toml);
    }
    bail!("workflow '{name}' not found in {}", dir.display())
}

fn load_workflow(path: &std::path::Path) -> Result<Workflow> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if path.extension().is_some_and(|e| e == "toml") {
        toml::from_str(&raw).with_context(|| format!("parse {} as TOML", path.display()))
    } else {
        serde_json::from_str(&raw).with_context(|| format!("parse {} as JSON", path.display()))
    }
}

pub async fn run_workflow(args: &WorkflowArgs) -> Result<()> {
    match &args.command {
        WorkflowCommand::Run(run_args) => run(run_args).await,
        WorkflowCommand::List => list(),
        WorkflowCommand::Show { name } => show(name),
        WorkflowCommand::New(new_args) => new(new_args),
    }
}

async fn run(args: &WorkflowRunArgs) -> Result<()> {
    let path = if let Some(file) = &args.file {
        file.clone()
    } else if let Some(name) = &args.name {
        resolve_workflow_path(name)?
    } else {
        bail!("workflow run requires --file or a workflow name");
    };
    let workflow = load_workflow(&path)?;
    if let Some(name) = &workflow.name {
        println!("workflow: {name}");
    }
    for (i, step) in workflow.step.iter().enumerate() {
        println!("-- step {i}: {}", step_name(step));
        if args.dry_run {
            continue;
        }
        run_step(step).await.with_context(|| format!("step {i}"))?;
    }
    Ok(())
}

fn list() -> Result<()> {
    let dir = workflows_dir()?;
    if !dir.exists() {
        println!("no workflows saved");
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Some(ext) = path.extension()
            && (ext == "json" || ext == "toml")
        {
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            println!("{name}");
        }
    }
    Ok(())
}

fn show(name: &str) -> Result<()> {
    let path = resolve_workflow_path(name)?;
    let workflow = load_workflow(&path)?;
    println!("{}", serde_json::to_string_pretty(&workflow)?);
    Ok(())
}

fn new(args: &WorkflowNewArgs) -> Result<()> {
    let dir = workflows_dir()?;
    std::fs::create_dir_all(&dir)?;
    let name = slugify(&args.name);
    let path = dir.join(format!("{name}.json"));
    let workflow = Workflow {
        name: Some(args.name.clone()),
        description: Some(args.description.clone()),
        step: vec![WorkflowStep::Exec(ExecStep {
            prompt: args.description.clone(),
            model: None,
            yolo: Some(true),
            tools: None,
            max_turns: None,
        })],
    };
    let raw = serde_json::to_string_pretty(&workflow)?;
    std::fs::write(&path, raw)?;
    println!("created workflow {name} at {}", path.display());
    Ok(())
}

fn step_name(step: &WorkflowStep) -> &'static str {
    match step {
        WorkflowStep::Exec(_) => "exec",
        WorkflowStep::FanOut(_) => "fan_out",
        WorkflowStep::Shell(_) => "shell",
    }
}

async fn run_step(step: &WorkflowStep) -> Result<()> {
    match step {
        WorkflowStep::Exec(s) => run_exec(s).await,
        WorkflowStep::FanOut(s) => run_fan_out(s).await,
        WorkflowStep::Shell(s) => run_shell(s).await,
    }
}

async fn run_exec(step: &ExecStep) -> Result<()> {
    let session = SessionParams::default();
    run_single_turn_with(
        &step.prompt,
        step.model.clone(),
        step.yolo.unwrap_or(true),
        OutputFormat::Plain,
        step.max_turns,
        step.tools.clone(),
        None,
        None,
        None,
        &session,
        false,
    )
    .await
}

async fn run_fan_out(step: &FanOutStep) -> Result<()> {
    const MAX_FAN_OUT: usize = 20;
    if step.count == 0 || step.count > MAX_FAN_OUT {
        bail!("fan_out count must be between 1 and {MAX_FAN_OUT}");
    }
    for i in 0..step.count {
        let prompt = format!("{}\n\nSubtask {}/{}", step.prompt, i + 1, step.count);
        let session = SessionParams::default();
        run_single_turn_with(
            &prompt,
            step.model.clone(),
            true,
            OutputFormat::Plain,
            step.max_turns,
            step.tools.clone(),
            None,
            None,
            None,
            &session,
            false,
        )
        .await?;
    }
    if let Some(aggregate) = &step.aggregate {
        let session = SessionParams::default();
        run_single_turn_with(
            aggregate,
            step.model.clone(),
            true,
            OutputFormat::Plain,
            step.max_turns,
            step.tools.clone(),
            None,
            None,
            None,
            &session,
            false,
        )
        .await?;
    }
    Ok(())
}

async fn run_shell(step: &ShellStep) -> Result<()> {
    let mut cmd = tokio::process::Command::new(&step.command);
    cmd.args(&step.args);
    let out = cmd.output().await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(expected) = step.expect_exit {
        if out.status.code() != Some(expected) {
            bail!("shell step exited {}; stderr: {stderr}", out.status);
        }
    } else if !out.status.success() {
        bail!("shell step failed: {stderr}");
    }
    if !stdout.is_empty() {
        println!("{stdout}");
    }
    Ok(())
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
        .replace("--", "-")
        .trim_matches('-')
        .to_string()
}
