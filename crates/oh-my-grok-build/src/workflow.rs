use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::args::{WorkflowArgs, WorkflowCommand, WorkflowNewArgs, WorkflowRunArgs};
use crate::{SessionParams, run_single_turn_with};
use xai_grok_pager::headless::OutputFormat;

const MAX_WORKFLOW_SIZE: u64 = 10 * 1024 * 1024;

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
    resolve_workflow_path_in(&workflows_dir()?, name)
}

fn reject_symlink(path: &std::path::Path) -> Result<()> {
    let meta =
        std::fs::symlink_metadata(path).with_context(|| format!("metadata {}", path.display()))?;
    if meta.is_symlink() {
        bail!("workflow path {} is a symlink", path.display());
    }
    Ok(())
}

fn resolve_workflow_path_in(dir: &std::path::Path, name: &str) -> Result<PathBuf> {
    let safe = slugify(name);
    if safe.is_empty() {
        bail!("invalid workflow name '{name}'");
    }
    let json = dir.join(format!("{safe}.json"));
    if json.exists() {
        reject_symlink(&json)?;
        return Ok(json);
    }
    let toml = dir.join(format!("{safe}.toml"));
    if toml.exists() {
        reject_symlink(&toml)?;
        return Ok(toml);
    }
    bail!("workflow '{name}' not found in {}", dir.display())
}

fn load_workflow(path: &std::path::Path) -> Result<Workflow> {
    let meta =
        std::fs::symlink_metadata(path).with_context(|| format!("metadata {}", path.display()))?;
    if !meta.is_file() || meta.is_symlink() {
        bail!("workflow {} is not a regular file", path.display());
    }
    if meta.len() > MAX_WORKFLOW_SIZE {
        bail!("workflow {} exceeds size limit", path.display());
    }
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

fn resolve_user_workflow_file(path: &std::path::Path) -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let workflows = workflows_dir()?;
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let canonical = dunce::canonicalize(&abs)
        .with_context(|| format!("workflow file not found: {}", abs.display()))?;
    let cwd_canonical =
        dunce::canonicalize(&cwd).unwrap_or_else(|_| dunce::simplified(&cwd).to_path_buf());
    let workflows_canonical = dunce::canonicalize(&workflows)
        .unwrap_or_else(|_| dunce::simplified(&workflows).to_path_buf());
    if !canonical.starts_with(&cwd_canonical) && !canonical.starts_with(&workflows_canonical) {
        bail!(
            "workflow --file must be under the current directory ({}) or {}",
            cwd_canonical.display(),
            workflows_canonical.display()
        );
    }
    reject_symlink(&abs)?;
    Ok(canonical)
}

async fn run(args: &WorkflowRunArgs) -> Result<()> {
    let path = if let Some(file) = &args.file {
        resolve_user_workflow_file(file)?
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
        run_step(step, args.allow_shell, args.yolo)
            .await
            .with_context(|| format!("step {i}"))?;
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
    if name.is_empty() {
        bail!("invalid workflow name '{}'", args.name);
    }
    let path = dir.join(format!("{name}.json"));
    let workflow = Workflow {
        name: Some(args.name.clone()),
        description: Some(args.description.clone()),
        step: vec![WorkflowStep::Exec(ExecStep {
            prompt: args.description.clone(),
            model: None,
            yolo: None,
            tools: None,
            max_turns: None,
        })],
    };
    let raw = serde_json::to_string_pretty(&workflow)?;
    crate::providers::write_file_atomic(&path, raw, true)?;
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

async fn run_step(step: &WorkflowStep, allow_shell: bool, run_yolo: bool) -> Result<()> {
    match step {
        WorkflowStep::Exec(s) => run_exec(s, run_yolo).await,
        WorkflowStep::FanOut(s) => run_fan_out(s, run_yolo).await,
        WorkflowStep::Shell(s) => run_shell(s, allow_shell).await,
    }
}

async fn run_exec(step: &ExecStep, run_yolo: bool) -> Result<()> {
    let session = SessionParams::default();
    let yolo = run_yolo && step.yolo.unwrap_or(true);
    run_single_turn_with(
        &step.prompt,
        step.model.clone(),
        yolo,
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

async fn run_fan_out(step: &FanOutStep, run_yolo: bool) -> Result<()> {
    const MAX_FAN_OUT: usize = 20;
    if step.count == 0 || step.count > MAX_FAN_OUT {
        bail!("fan_out count must be between 1 and {MAX_FAN_OUT}");
    }
    let yolo = run_yolo && step.yolo.unwrap_or(true);
    for i in 0..step.count {
        let prompt = format!("{}\n\nSubtask {}/{}", step.prompt, i + 1, step.count);
        let session = SessionParams::default();
        run_single_turn_with(
            &prompt,
            step.model.clone(),
            yolo,
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
            yolo,
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

async fn run_shell(step: &ShellStep, allow_shell: bool) -> Result<()> {
    if !allow_shell {
        bail!("shell step blocked: rerun with --allow-shell to execute arbitrary commands");
    }
    crate::playbook::run_shell_step(&step.command, &step.args, step.expect_exit)
        .await
        .context("workflow shell step")
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
        .replace("--", "-")
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn slugify_normalizes_names() {
        assert_eq!(slugify("My Workflow"), "my-workflow");
        assert_eq!(slugify("CI_checks"), "ci_checks");
        assert_eq!(slugify("foo/bar"), "foo-bar");
        assert_eq!(slugify("..\\Cargo.toml"), "cargo-toml");
        assert_eq!(slugify("!@#"), "");
    }

    #[test]
    fn resolve_workflow_path_blocks_traversal() {
        let tmp = std::env::temp_dir().join(format!("omgb-wf-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let valid = tmp.join("good.json");
        std::fs::File::create(&valid)
            .unwrap()
            .write_all(b"{}")
            .unwrap();

        // Path-separator characters are normalized, not interpreted as directories.
        assert_eq!(resolve_workflow_path_in(&tmp, "../good").unwrap(), valid);

        // Empty sanitized names are rejected.
        assert!(resolve_workflow_path_in(&tmp, "!@#").is_err());

        // Missing workflows are reported, not silently resolved outside the dir.
        assert!(resolve_workflow_path_in(&tmp, "missing").is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[cfg(unix)]
    fn resolve_workflow_path_rejects_symlink() {
        let tmp = std::env::temp_dir().join(format!("omgb-wf-symlink-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let target = tmp.join("real.json");
        std::fs::File::create(&target)
            .unwrap()
            .write_all(b"{}")
            .unwrap();
        let link = tmp.join("good.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(resolve_workflow_path_in(&tmp, "good").is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
