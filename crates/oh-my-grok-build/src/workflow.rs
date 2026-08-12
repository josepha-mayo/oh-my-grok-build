use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::args::{
    WorkflowArgs, WorkflowCommand, WorkflowCreateArgs, WorkflowNewArgs, WorkflowRunArgs,
};
use crate::group::normalize_model;
use crate::{SessionParams, run_single_turn_with};
use xai_grok_pager::headless::OutputFormat;

const MAX_WORKFLOW_SIZE: u64 = 10 * 1024 * 1024;
const MAX_FAN_OUT: usize = 1024;
const MAX_FAN_OUT_CONTEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_FAN_OUT_RESULT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Workflow {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    step: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WorkflowStep {
    Exec(ExecStep),
    FanOut(FanOutStep),
    Shell(ShellStep),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        WorkflowCommand::Create(create_args) => create(create_args).await,
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
    let mut workflow = load_workflow(&path)?;
    if !args.args.is_empty() {
        workflow.step = workflow
            .step
            .iter()
            .map(|s| substitute_step_args(s, &args.args))
            .collect();
    }
    smoke_check_workflow(&workflow).context("workflow validation failed after substitution")?;
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

fn write_new_workflow(path: &std::path::Path, raw: &[u8]) -> Result<()> {
    if raw.len() as u64 > MAX_WORKFLOW_SIZE {
        bail!("workflow exceeds the {MAX_WORKFLOW_SIZE} byte limit");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("workflow path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let lock_path = path.with_extension("create.lock");
    if let Ok(metadata) = std::fs::symlink_metadata(&lock_path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        bail!("workflow creation lock is not a regular file");
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    crate::providers::restrict_omg_file_permissions(&lock_path)?;
    lock.lock_exclusive()?;
    if path.exists() {
        bail!("workflow already exists: {}", path.display());
    }
    crate::providers::write_file_atomic(path, raw, true)
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
    write_new_workflow(&path, raw.as_bytes())?;
    println!("created workflow {name} at {}", path.display());
    Ok(())
}

pub(crate) async fn create_workflow(args: &WorkflowCreateArgs) -> Result<(String, PathBuf)> {
    let model = match &args.model {
        Some(m) => normalize_model(m),
        None => crate::resolve_model_candidates(&args.prompt, None)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no model is available for workflow creation"))?,
    };
    if !crate::group::is_known_group_model(&model) {
        bail!(
            "unknown workflow model '{model}'; pass a provider id (e.g. xai, openai) or known model name"
        );
    }

    let name = args
        .name
        .clone()
        .unwrap_or_else(|| derive_workflow_name(&args.prompt));
    let safe_name = slugify(&name);
    if safe_name.is_empty() {
        bail!("invalid workflow name '{name}'");
    }

    let prompt = format!(
        "Create an `omgb` workflow JSON for the following task. \
         The workflow may use 'exec' (run a model prompt), 'fan_out' (spawn parallel agents), \
         and 'shell' (run a command) step types. Include verification or aggregation steps where sensible.\n\n\
         Task: {prompt}\n\n\
         Output strictly valid JSON matching this shape (no markdown, no code fences):\n\
         {{\"name\":\"Workflow Name\",\"description\":\"...\",\"step\":[{{\"type\":\"exec\",\"prompt\":\"...\"}},{{\"type\":\"fan_out\",\"prompt\":\"...\",\"count\":3}},{{\"type\":\"shell\",\"command\":\"echo\",\"args\":[\"ok\"]}}]}}\n\n\
         The 'exec' step may include optional fields: model (provider id or known model name), yolo (bool), tools (comma-separated string), max_turns (u32, defaults to 10). \
         The 'fan_out' step may include the same plus count (required, 1-1024) and aggregate (string prompt); max_turns defaults to 5. \
         The 'shell' step may include args (array of strings) and expect_exit (i32).",
        prompt = args.prompt
    );

    let reply =
        crate::run_single_turn_capture(&prompt, Some(model), args.yolo, Some(1), None).await?;
    let value = crate::extract_json_object(&reply)
        .ok_or_else(|| anyhow::anyhow!("workflow create did not return valid JSON"))?;
    let mut workflow: Workflow = serde_json::from_value(value)
        .with_context(|| format!("generated workflow is not valid: {reply}"))?;

    if workflow.step.is_empty() {
        bail!("generated workflow has no steps");
    }

    workflow.name = Some(workflow.name.unwrap_or(name));
    workflow.description = Some(workflow.description.unwrap_or_else(|| args.prompt.clone()));

    let summary = smoke_check_workflow(&workflow)?;

    if args.dry_run {
        println!("{summary}\n");
        println!("{}", serde_json::to_string_pretty(&workflow)?);
        return Ok((safe_name, PathBuf::new()));
    }

    let dir = workflows_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{safe_name}.json"));
    let raw = serde_json::to_vec_pretty(&workflow)?;
    write_new_workflow(&path, &raw)?;
    println!("{summary}");
    println!("created workflow {safe_name} at {}", path.display());
    Ok((safe_name, path))
}

async fn create(args: &WorkflowCreateArgs) -> Result<()> {
    create_workflow(args).await?;
    Ok(())
}

fn smoke_check_workflow(workflow: &Workflow) -> Result<String> {
    let mut summary = format!(
        "Workflow '{}'",
        workflow.name.as_deref().unwrap_or("unnamed")
    );
    if let Some(desc) = workflow.description.as_deref() {
        summary.push_str(&format!(": {desc}"));
    }
    summary.push_str(&format!(" — {} step(s)", workflow.step.len()));

    for (i, step) in workflow.step.iter().enumerate() {
        match step {
            WorkflowStep::Exec(s) => {
                if s.prompt.trim().is_empty() {
                    bail!("step {i}: exec prompt must not be empty");
                }
                let preview: String = s
                    .prompt
                    .split_whitespace()
                    .take(8)
                    .collect::<Vec<_>>()
                    .join(" ");
                summary.push_str(&format!("\n  {i}: exec — {preview}"));
            }
            WorkflowStep::FanOut(s) => {
                if s.count == 0 || s.count > MAX_FAN_OUT {
                    bail!("step {i}: fan_out count must be between 1 and {MAX_FAN_OUT}");
                }
                if s.prompt.trim().is_empty() {
                    bail!("step {i}: fan_out prompt must not be empty");
                }
                let agg = if s.aggregate.is_some() {
                    " + aggregate"
                } else {
                    ""
                };
                summary.push_str(&format!("\n  {i}: fan_out x{}{agg}", s.count));
            }
            WorkflowStep::Shell(s) => {
                if s.command.trim().is_empty() {
                    bail!("step {i}: shell command must not be empty");
                }
                let args = s.args.join(" ");
                summary.push_str(&format!("\n  {i}: shell {} {args}", s.command));
            }
        }
    }
    Ok(summary)
}

fn derive_workflow_name(prompt: &str) -> String {
    prompt
        .split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
        .replace("--", "-")
        .trim_matches('-')
        .to_string()
}

fn substitute_args(template: &str, args: &[String]) -> String {
    let mut out = template.to_string();
    for (i, a) in args.iter().enumerate() {
        out = out.replace(&format!("{{{{{}}}}}", i), a);
    }
    if !args.is_empty() {
        out = out.replace("{{args}}", &args.join(" "));
    }
    out
}

fn substitute_step_args(step: &WorkflowStep, args: &[String]) -> WorkflowStep {
    match step {
        WorkflowStep::Exec(s) => {
            let mut s = s.clone();
            s.prompt = substitute_args(&s.prompt, args);
            WorkflowStep::Exec(s)
        }
        WorkflowStep::FanOut(s) => {
            let mut s = s.clone();
            s.prompt = substitute_args(&s.prompt, args);
            s.aggregate = s.aggregate.as_deref().map(|a| substitute_args(a, args));
            WorkflowStep::FanOut(s)
        }
        WorkflowStep::Shell(s) => {
            let mut s = s.clone();
            s.command = substitute_args(&s.command, args);
            s.args = s.args.iter().map(|a| substitute_args(a, args)).collect();
            WorkflowStep::Shell(s)
        }
    }
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
    let model = step.model.as_deref().map(normalize_model);
    if let Some(ref m) = model
        && !crate::group::is_known_group_model(m)
    {
        bail!("workflow exec step references unknown model '{m}'");
    }
    run_single_turn_with(
        &step.prompt,
        model,
        yolo,
        OutputFormat::Plain,
        step.max_turns.or(Some(10)),
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
    const MAX_CONCURRENT: usize = 20;
    if step.count == 0 || step.count > MAX_FAN_OUT {
        bail!("fan_out count must be between 1 and {MAX_FAN_OUT}");
    }
    let yolo = run_yolo && step.yolo.unwrap_or(true);
    let model = step.model.as_deref().map(normalize_model);
    if let Some(ref m) = model
        && !crate::group::is_known_group_model(m)
    {
        bail!("workflow fan_out step references unknown model '{m}'");
    }
    let max_turns = step.max_turns.or(Some(5));
    let mut outputs: Vec<(usize, String)> = Vec::with_capacity(step.count);
    let mut failures = Vec::new();
    let mut output_bytes = 0_usize;

    let mut i = 0;
    while i < step.count {
        let end = (i + MAX_CONCURRENT).min(step.count);
        let futures = (i..end).map(|j| {
            let prompt = format!(
                "{}\n\nSubtask {}/{}\n\nProduce a concise result for this subtask.",
                step.prompt,
                j + 1,
                step.count
            );
            let m = model.clone();
            let tools = step.tools.clone();
            async move {
                crate::run_single_turn_capture_with_limit(
                    &prompt,
                    m,
                    yolo,
                    max_turns,
                    tools,
                    MAX_FAN_OUT_RESULT_BYTES,
                )
                .await
            }
        });
        let results = futures::future::join_all(futures).await;
        for (offset, res) in results.into_iter().enumerate() {
            let n = i + offset + 1;
            match res {
                Ok(text) => {
                    let separator_bytes = usize::from(!outputs.is_empty()) * 2;
                    let rendered_bytes = format!("--- Subtask {n} result ---\n").len()
                        + text.len()
                        + separator_bytes;
                    output_bytes = output_bytes.saturating_add(rendered_bytes);
                    if output_bytes > MAX_FAN_OUT_CONTEXT_BYTES {
                        bail!(
                            "fan_out results exceed the {MAX_FAN_OUT_CONTEXT_BYTES} byte aggregation/display limit"
                        );
                    }
                    outputs.push((n, text));
                }
                Err(e) => {
                    eprintln!("warning: fan_out subtask {n} failed: {e}");
                    failures.push(n);
                }
            }
        }
        i = end;
    }

    if !failures.is_empty() {
        bail!(
            "fan_out failed: {} of {} subtasks did not complete",
            failures.len(),
            step.count
        );
    }
    if outputs.is_empty() {
        bail!("fan_out produced no results");
    }

    let context = format_fan_out_outputs(&outputs);
    debug_assert!(context.len() <= MAX_FAN_OUT_CONTEXT_BYTES);
    if let Some(aggregate) = &step.aggregate {
        let prompt = format!("{aggregate}\n\n{context}");
        let result =
            crate::run_single_turn_capture(&prompt, model, yolo, max_turns, step.tools.clone())
                .await
                .context("fan_out aggregate failed")?;
        println!("{result}");
    } else {
        println!("{context}");
    }
    Ok(())
}

fn format_fan_out_outputs(outputs: &[(usize, String)]) -> String {
    outputs
        .iter()
        .map(|(i, text)| format!("--- Subtask {i} result ---\n{text}"))
        .collect::<Vec<_>>()
        .join("\n\n")
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
    fn fan_out_results_are_rendered_in_task_order() {
        let rendered = format_fan_out_outputs(&[
            (1, "first".into()),
            (2, "second".into()),
            (3, "third".into()),
        ]);
        assert_eq!(
            rendered,
            "--- Subtask 1 result ---\nfirst\n\n--- Subtask 2 result ---\nsecond\n\n--- Subtask 3 result ---\nthird"
        );
    }

    #[test]
    fn new_workflow_does_not_overwrite_an_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing.json");
        std::fs::write(&path, b"original").unwrap();
        assert!(write_new_workflow(&path, b"replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }

    #[test]
    fn concurrent_workflow_creators_have_exactly_one_winner() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shared.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for raw in [b"first".as_slice(), b"second".as_slice()] {
            let path = path.clone();
            let barrier = barrier.clone();
            let raw = raw.to_vec();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                write_new_workflow(&path, &raw).is_ok()
            }));
        }
        barrier.wait();
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        let saved = std::fs::read(path).unwrap();
        assert!(saved == b"first" || saved == b"second");
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
