//! `oh-my-grok-build` / `omgb` composition-root binary.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::Parser;
use tokio::process::Command;

use xai_grok_pager::app::{PagerArgs, run as pager_run};
use xai_grok_pager::headless::{HeadlessOptions, HeadlessPrompt, OutputFormat, run_single_turn};
use xai_grok_shell::agent::config::Config as AgentConfig;

mod args;
mod harness;
mod memory;
mod moe;
mod net;
mod providers;
mod research;
mod scheduler;
mod server;
mod session;
mod subagents;
mod swarm;
mod taste;
mod timeline;

use args::*;

fn desktop_control_allowed() -> bool {
    std::env::var("OMGB_ALLOW_DESKTOP_CONTROL")
        .is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
}

/// Loads valid `*_API_KEY` entries from `~/.omgb/.env` into the process
/// environment, including keys referenced by configured providers/connectors
/// plus any valid `*_API_KEY` entries already present in the file. This lets
/// catalog-based MoE routing discover keys before a provider has been persisted
/// to config.
///
/// This is a bridge to the upstream Grok Build harness, which reads provider
/// secrets from the process environment via `env_key`. It is called only before
/// any other thread starts.
///
/// # Safety
/// Must be called before any other thread can read the environment. This is the
/// first thing `main()` does, before installing signal handlers or spawning the
/// Tokio runtime.
unsafe fn load_omg_env_into_process() -> Result<()> {
    let entries = crate::providers::load_env_file()?;
    let allowed = crate::providers::env_keys_to_load();
    for (k, v) in entries {
        let relevant = allowed.contains(&k) || k == "OMGB_API_KEY";
        if relevant && crate::providers::is_valid_env_key(&k) && !v.is_empty() {
            // SAFETY: see the function-level safety contract above.
            unsafe { std::env::set_var(k, v) };
        }
    }
    Ok(())
}

pub fn main() -> Result<()> {
    // Load valid *_API_KEY entries from ~/.omgb/.env before anything else can read
    // the process environment. This is safe because it is the very first
    // operation and runs before any other thread or signal handler is installed.
    // SAFETY: no other threads exist at this point.
    unsafe { load_omg_env_into_process() }?;

    let cli = OmgbArgs::parse();

    if let Some(OmgbCommand::Autonomous(args)) = cli.command.as_ref() {
        let cwd = std::env::current_dir()?;
        xai_grok_shell::config::apply_sandbox(
            None,
            Some(args.sandbox_profile.as_str()),
            Some(&cwd),
        );
    }

    xai_grok_pager_minimal::install();
    xai_crash_handler::install_terminal_restore_only();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(cli))
}

async fn async_main(cli: OmgbArgs) -> Result<()> {
    let command = cli
        .command
        .unwrap_or_else(|| OmgbCommand::Tui(TuiArgs::default()));

    match command {
        OmgbCommand::Tui(args) => run_tui(args).await,
        OmgbCommand::Exec(args) => run_exec(args).await,
        OmgbCommand::Loop(args) => run_loop(args).await,
        OmgbCommand::Autonomous(args) => run_autonomous(args).await,
        OmgbCommand::Provider(args) => run_provider(args).await,
        OmgbCommand::Model(args) => run_model(args).await,
        OmgbCommand::Cron(args) => run_cron(args).await,
        OmgbCommand::Schedule(args) => run_schedule(args).await,
        OmgbCommand::Team(args) => run_team(args).await,
        OmgbCommand::Swarm(args) => run_swarm(args).await,
        OmgbCommand::Subagent(args) => run_subagent(args).await,
        OmgbCommand::Research(args) => {
            research::run_research(&args.topic, args.count, args.model, args.yolo, args.output)
                .await
        }
        OmgbCommand::Session(args) => session::run_session(args).await,
        OmgbCommand::Memory(args) => memory::run_memory(args),
        OmgbCommand::Timeline(args) => timeline::list_events(args.limit, args.json),
        OmgbCommand::Harness(args) => run_harness(args).await,
        OmgbCommand::Serve(args) => server::serve(&args).await,
        OmgbCommand::Connect(args) => server::connect(&args).await,
        OmgbCommand::Use(args) => run_use(args).await,
        OmgbCommand::Browser(args) => run_browser(args).await,
        OmgbCommand::Mcp(args) => xai_grok_pager::mcp_cmd::run(args).await,
        OmgbCommand::Taste(args) => run_taste(args),
        OmgbCommand::Commit(args) => run_commit(args).await,
        OmgbCommand::Review => run_review().await,
        OmgbCommand::Undo(args) => run_undo(args).await,
    }
}

pub(crate) fn build_agent_config(model: Option<String>) -> Result<AgentConfig> {
    let raw = xai_grok_shell::config::load_effective_config_disk_only()
        .map_err(|e| anyhow::anyhow!("failed to load config: {e}"))?;
    let mut cfg = AgentConfig::new_from_toml_cfg(&raw)
        .map_err(|e| anyhow::anyhow!("failed to create agent config: {e}"))?;
    cfg.default_model_override = model;
    Ok(cfg)
}

fn config_sandbox_profile() -> Option<String> {
    let raw = xai_grok_shell::config::load_effective_config_disk_only().ok()?;
    raw.get("sandbox")?
        .get("profile")?
        .as_str()
        .map(|s| s.to_string())
}

async fn run_tui(args: TuiArgs) -> Result<()> {
    let mut argv = vec!["omgb".to_string()];
    if let Some(m) = args.model {
        argv.push("--model".to_string());
        argv.push(m);
    }
    if let Some(sid) = args.session.session_id {
        argv.push("--session-id".to_string());
        argv.push(sid);
    }
    if let Some(r) = args.session.resume {
        argv.push("--resume".to_string());
        if !r.is_empty() {
            argv.push(r);
        }
    } else if args.session.continue_last {
        argv.push("--continue".to_string());
    }
    if args.session.fork_session {
        argv.push("--fork-session".to_string());
    }
    if let Some(p) = args.prompt {
        argv.push("--".to_string());
        argv.push(p);
    }
    let pager_args = PagerArgs::parse_from(argv);
    pager_run(pager_args, None).await?;
    Ok(())
}

fn resolve_path(raw: &std::path::Path) -> Result<std::path::PathBuf> {
    let mut has_normal = false;
    for comp in raw.components() {
        match comp {
            std::path::Component::Normal(_) => has_normal = true,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                bail!("path must not contain '..' components: {}", raw.display())
            }
            _ => {}
        }
    }
    if !has_normal {
        bail!(
            "path must contain at least one file or directory component: {}",
            raw.display()
        );
    }
    if raw.is_absolute() {
        Ok(raw.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(raw))
    }
}

pub(crate) async fn write_prompt_temp(prompt: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("omgb-prompt-{}.txt", uuid::Uuid::new_v4()));
    tokio::fs::write(&path, prompt.as_bytes()).await?;
    let path2 = path.clone();
    tokio::task::spawn_blocking(move || restrict_temp_permissions(&path2)).await??;
    Ok(path)
}

#[cfg(unix)]
fn restrict_temp_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(windows)]
fn restrict_temp_permissions(_path: &std::path::Path) -> Result<()> {
    // Windows TEMP is already per-user; no Unix-style mode setting.
    Ok(())
}

pub(crate) struct PromptFileGuard(PathBuf);
impl Drop for PromptFileGuard {
    fn drop(&mut self) {
        let path = std::mem::take(&mut self.0);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            std::mem::drop(rt.spawn_blocking(move || std::fs::remove_file(&path)));
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

pub(crate) fn spawn_detached(
    mut cmd: tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    #[cfg(unix)]
    xai_tty_utils::detach_command(&mut cmd);
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;
        let base = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
        cmd.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.raw_os_error() == Some(5) => {
                // Parent job does not allow breakaway; retry without it.
                // We will not be able to assign the child to our own JobObject,
                // but the command still runs and can be killed directly.
                cmd.creation_flags(base);
            }
            Err(e) => return Err(e),
        }
    }
    cmd.spawn()
}

pub(crate) fn spawn_with_process_group(
    cmd: tokio::process::Command,
) -> Result<(tokio::process::Child, Option<xai_tty_utils::ProcessGroup>)> {
    let child = spawn_detached(cmd)?;
    let group = match xai_tty_utils::ProcessGroup::new() {
        Ok(mut g) => match g.attach(&child) {
            Ok(()) => Some(g),
            Err(_) => None,
        },
        Err(_) => None,
    };
    Ok((child, group))
}

pub(crate) fn kill_process_group(group: Option<&xai_tty_utils::ProcessGroup>) {
    if let Some(g) = group {
        let _ = g.kill();
    }
}

pub(crate) async fn kill_child_and_reap(
    child: &mut tokio::process::Child,
    group: Option<&xai_tty_utils::ProcessGroup>,
) {
    if let Some(g) = group {
        let _ = g.kill();
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(unix)]
pub(crate) fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
pub(crate) fn process_alive(pid: u32) -> bool {
    let Ok(output) = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
    else {
        return false;
    };
    let pid_s = pid.to_string();
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().any(|line| {
        let mut parts = line.split("\",\"");
        let Some(pid_field) = parts.nth(1) else {
            return false;
        };
        pid_field.trim_matches('"') == pid_s
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn process_alive(_pid: u32) -> bool {
    true
}

async fn run_exec(args: ExecArgs) -> Result<()> {
    let output_path = args.output_file.as_deref().map(resolve_path).transpose()?;
    let prompt_file = if let Some(p) = &args.prompt_file {
        let p = resolve_path(p)?;
        if !p.is_file() {
            bail!(
                "prompt file does not exist or is not a file: {}",
                p.display()
            );
        }
        Some(p)
    } else {
        None
    };

    let prompt = if let Some(p) = &prompt_file {
        tokio::fs::read_to_string(p)
            .await
            .map_err(|e| anyhow::anyhow!("failed to read prompt file: {e}"))?
    } else if let Some(p) = &args.prompt {
        p.clone()
    } else {
        bail!("prompt is required")
    };

    if let Some(path) = output_path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let own_prompt = args.prompt_file_own || prompt_file.is_none();
        let child_prompt_file = if let Some(p) = prompt_file {
            p
        } else {
            write_prompt_temp(&prompt).await?
        };
        let _prompt_guard = if own_prompt {
            Some(PromptFileGuard(child_prompt_file.clone()))
        } else {
            None
        };
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("exec")
            .arg("--prompt-file")
            .arg(&child_prompt_file)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(m) = &args.model {
            cmd.arg("--model").arg(m);
        }
        if args.yolo {
            cmd.arg("--yolo");
        }
        if args.json {
            cmd.arg("--json");
        }
        if let Some(t) = &args.tools {
            cmd.arg("--tools").arg(t);
        }
        if let Some(dt) = &args.disallowed_tools {
            cmd.arg("--disallowed-tools").arg(dt);
        }
        let out = cmd.output().await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("exec failed: {stderr}");
        }
        std::fs::write(&path, &out.stdout)?;
        println!("wrote output to {}", path.display());
        if args.commit || args.commit_untracked {
            git_commit_all("omgb exec", args.commit_untracked, Some(path.as_path())).await?;
        }
        return Ok(());
    }

    let _prompt_guard = if args.prompt_file_own {
        prompt_file.as_ref().map(|p| PromptFileGuard(p.clone()))
    } else {
        None
    };

    run_single_turn_with(
        &prompt,
        args.model.clone(),
        args.yolo,
        if args.json {
            OutputFormat::Json
        } else {
            OutputFormat::Plain
        },
        None,
        args.tools.clone(),
        args.disallowed_tools.clone(),
        None,
        None,
        &args.session,
        args.memory,
    )
    .await?;
    if args.commit || args.commit_untracked {
        git_commit_all("omgb exec", args.commit_untracked, None).await?;
    }
    Ok(())
}

async fn run_autonomous(args: AutonomousArgs) -> Result<()> {
    if !args.yolo {
        bail!("autonomous mode requires --yolo to auto-approve tool use");
    }
    if matches!(
        config_sandbox_profile().as_deref(),
        None | Some("") | Some("off")
    ) {
        eprintln!(
            "warning: autonomous mode should run inside a sandbox; \
             [sandbox].profile is unset or 'off' in ~/.grok/config.toml"
        );
    }
    let prompt = format!(
        "{prompt}\n\nRun autonomously. Sandbox profile: {profile}.",
        prompt = args.prompt,
        profile = args.sandbox_profile
    );
    run_single_turn_with(
        &prompt,
        args.model,
        args.yolo,
        OutputFormat::Plain,
        Some(50),
        Some("run_terminal_cmd,read_file,search_replace,grep,list_dir".to_string()),
        None,
        None,
        None,
        &args.session,
        args.memory,
    )
    .await
}

async fn run_use(args: UseArgs) -> Result<()> {
    if !args.yolo {
        bail!("`omgb use` requires --yolo to auto-approve tool use");
    }
    if !desktop_control_allowed() {
        bail!("desktop control requires OMGB_ALLOW_DESKTOP_CONTROL=1/true/yes/on");
    }
    let prompt = format!("{}\n\nUse the computer as needed.", args.prompt);
    run_single_turn_with(
        &prompt,
        args.model,
        args.yolo,
        OutputFormat::Plain,
        None,
        Some("run_terminal_cmd,read_file,search_replace,grep,list_dir".to_string()),
        None,
        None,
        None,
        &SessionParams::default(),
        false,
    )
    .await
}

async fn run_browser(args: BrowserArgs) -> Result<()> {
    if !args.yolo {
        bail!("`omgb browser` requires --yolo to auto-approve tool use");
    }
    if !desktop_control_allowed() {
        bail!("desktop control requires OMGB_ALLOW_DESKTOP_CONTROL=1/true/yes/on");
    }
    let mut prompt = args.prompt.clone();
    if let Some(url) = args.url {
        crate::net::validate_url(&url, args.allow_local, args.allow_private).await?;
        prompt.push_str(&format!("\n\nStart at URL: {url}. Do not navigate to a different origin unless the task explicitly requires it."));
    }
    prompt.push_str("\n\nUse the browser/computer as needed.");
    run_single_turn_with(
        &prompt,
        args.model,
        args.yolo,
        OutputFormat::Plain,
        None,
        None,
        None,
        Some("browser-use".to_string()),
        None,
        &SessionParams::default(),
        false,
    )
    .await
}

pub(crate) async fn run_single_turn_with(
    prompt: &str,
    model: Option<String>,
    yolo: bool,
    output_format: OutputFormat,
    max_turns: Option<u32>,
    cli_tools: Option<String>,
    cli_disallowed_tools: Option<String>,
    agent: Option<String>,
    cwd: Option<PathBuf>,
    session: &SessionParams,
    memory: bool,
) -> Result<()> {
    let full_prompt = format!("{}{}", prompt, taste::taste_preamble());
    let model = if let Some(m) = model {
        Some(m)
    } else if let Ok(id) = moe::select_provider(prompt) {
        providers::ensure_provider_configured(&id)?;
        Some(format!("omgb-{id}"))
    } else {
        None
    };
    let rules = if memory {
        crate::memory::recall_for_prompt(prompt, 5).ok()
    } else {
        None
    };
    let options = HeadlessOptions {
        session_id: session.session_id.clone(),
        resume: session.resume.clone(),
        cwd,
        yolo,
        trust: yolo,
        output_format,
        json_schema: None,
        model,
        rules,
        system_prompt_override: None,
        continue_last_session: session.continue_last,
        fork_session: session.fork_session,
        worktree: None,
        restore_code: false,
        agent,
        agents_json: None,
        cli_tools,
        cli_disallowed_tools,
        disable_web_search: false,
        allow_rules: Vec::new(),
        deny_rules: Vec::new(),
        max_turns,
        permission_mode_flag: None,
        reasoning_effort: None,
        self_verify: false,
        best_of_n: None,
        wait_for_background: true,
        background_wait_timeout: Duration::from_secs(300),
    };

    let _ = timeline::add_event(
        "exec",
        prompt
            .split_whitespace()
            .take(8)
            .collect::<Vec<_>>()
            .join(" "),
        None,
    );

    run_single_turn(HeadlessPrompt::Text(full_prompt), false, options).await
}

async fn git_worktree_status() -> Result<(bool, String)> {
    let out = Command::new("git")
        .args(["status", "--short"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;
    if !out.status.success() {
        bail!("git status failed; this command requires a git repository");
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    Ok((text.trim().is_empty(), text))
}

async fn git_diff_text() -> Result<String> {
    let out = Command::new("git")
        .args(["diff", "--no-ext-diff", "--no-color"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;
    if !out.status.success() {
        bail!("git diff failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn git_config(key: &str) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(["config", "--get", key])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;
    if !out.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(if text.is_empty() { None } else { Some(text) })
}

async fn git_author() -> Result<(String, String)> {
    if let (Ok(name), Ok(email)) = (
        std::env::var("OMGB_GIT_AUTHOR_NAME"),
        std::env::var("OMGB_GIT_AUTHOR_EMAIL"),
    ) && !name.is_empty()
        && !email.is_empty()
    {
        return Ok((name, email));
    }
    let name = git_config("user.name").await?.ok_or_else(|| {
        anyhow::anyhow!("git author name not configured; set user.name or OMGB_GIT_AUTHOR_NAME")
    })?;
    let email = git_config("user.email").await?.ok_or_else(|| {
        anyhow::anyhow!("git author email not configured; set user.email or OMGB_GIT_AUTHOR_EMAIL")
    })?;
    Ok((name, email))
}

async fn git_commit_all(
    message: &str,
    include_untracked: bool,
    extra_path: Option<&std::path::Path>,
) -> Result<()> {
    if let Some(path) = extra_path {
        let add = Command::new("git")
            .args(["add", "--"])
            .arg(path)
            .status()
            .await?;
        if !add.success() {
            bail!("git add failed for {}", path.display());
        }
    }

    let (clean, status_text) = git_worktree_status().await?;
    if clean {
        return Ok(());
    }
    let has_untracked = status_text.lines().any(|l| l.starts_with("??"));
    if !include_untracked && has_untracked {
        bail!("working tree has untracked files; stage them or use --commit-untracked");
    }

    let add_flag = if include_untracked { "-A" } else { "-u" };
    let add = Command::new("git").args(["add", add_flag]).status().await?;
    if !add.success() {
        bail!("git add failed");
    }

    let (name, email) = git_author().await?;
    let commit = Command::new("git")
        .env("GIT_AUTHOR_NAME", &name)
        .env("GIT_AUTHOR_EMAIL", &email)
        .env("GIT_COMMITTER_NAME", &name)
        .env("GIT_COMMITTER_EMAIL", &email)
        .args(["commit", "-m", message, "--no-gpg-sign"])
        .status()
        .await?;
    if !commit.success() {
        bail!("git commit failed");
    }
    Ok(())
}

async fn run_commit(args: CommitArgs) -> Result<()> {
    let message = args.message.unwrap_or_else(|| "omgb commit".into());
    git_commit_all(&message, args.untracked, None).await
}

async fn run_review() -> Result<()> {
    let (clean, status_text) = git_worktree_status().await?;
    let diff_text = git_diff_text().await?;
    if clean {
        println!("working tree clean");
    } else {
        println!("Status:\n{status_text}");
    }
    if !diff_text.is_empty() {
        println!("\nDiff:\n{diff_text}");
    }
    Ok(())
}

async fn git_repo_root() -> Result<std::path::PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;
    if !out.status.success() {
        bail!("not inside a git repository");
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(root.into())
}

async fn run_undo(args: UndoArgs) -> Result<()> {
    let mode = if args.hard { "--hard" } else { "--soft" };
    let root = git_repo_root().await?;
    let out = Command::new("git")
        .current_dir(&root)
        .args(["reset", mode, "HEAD~1"])
        .output()
        .await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git reset failed: {stderr}");
    }
    if args.hard {
        println!("undone last commit and discarded working tree changes");
    } else {
        println!("undone last commit; changes are staged in the working tree");
    }
    Ok(())
}

async fn run_loop(args: LoopArgs) -> Result<()> {
    const MAX_DIFF_CHARS: usize = 16 * 1024;

    if !args.yolo {
        bail!("`omgb loop` requires --yolo to auto-approve tool use");
    }

    if !git_worktree_status().await?.0 {
        bail!("git working tree is not clean; commit or stash changes before running `omgb loop`");
    }

    let mut iteration = 0;
    let mut prompt = args.prompt.clone();
    let mut clean = true;
    let mut status;
    while iteration < args.max_iterations {
        iteration += 1;
        println!("\n--- iteration {iteration} ---");
        run_single_turn_with(
            &prompt,
            args.model.clone(),
            args.yolo,
            OutputFormat::Plain,
            None,
            None,
            None,
            None,
            None,
            &args.session,
            args.memory,
        )
        .await?;

        (clean, status) = git_worktree_status().await?;
        if clean {
            println!("worktree clean; stopping loop.");
            break;
        }
        let mut diff = git_diff_text().await?;
        if diff.len() > MAX_DIFF_CHARS {
            let mut end = MAX_DIFF_CHARS;
            while !diff.is_char_boundary(end) {
                end -= 1;
            }
            diff = format!("{}...\n(truncated)", &diff[..end]);
        }
        let changes = if diff.trim().is_empty() {
            status
        } else {
            format!("{status}\n{diff}")
        };
        prompt = format!(
            "Original task: {}\n\nCurrent git changes:\n{}\n\nContinue until complete.",
            args.prompt, changes
        );
    }
    if !clean {
        if args.commit || args.commit_untracked {
            git_commit_all("omgb loop", args.commit_untracked, None).await?;
            println!("committed loop changes.");
        } else {
            bail!(
                "loop finished with uncommitted changes; pass --commit or --commit-untracked to commit them"
            );
        }
    }
    Ok(())
}

async fn run_provider(args: ProviderArgs) -> Result<()> {
    use providers::*;
    match args.command {
        ProviderCommand::List => {
            for p in list_providers()? {
                let default = if Some(format!("omgb-{}", p.id)) == load_omg_config()?.default_model
                {
                    " (default)"
                } else {
                    ""
                };
                println!("{}{} - {} -> {}", p.id, default, p.name, p.base_url);
            }
        }
        ProviderCommand::Add(add_args) => {
            let p = add_provider(&add_args).await?;
            println!("added provider {} ({}) -> {}", p.id, p.name, p.base_url);
        }
        ProviderCommand::Remove { id } => {
            remove_provider(&id)?;
            println!("removed provider {id}");
        }
        ProviderCommand::Discover(discover_args) => {
            let found = discover_local_models(&discover_args).await?;
            for (provider, _url, models) in &found {
                let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
                println!("{provider}: {}", ids.join(", "));
            }
            if discover_args.add {
                add_discovered_providers(&found)?;
                println!("added discovered providers");
            }
        }
        ProviderCommand::Test { id } => {
            let (ok, err) = test_provider(&id).await?;
            if ok {
                println!("provider {id}: ok");
            } else {
                bail!("provider {id} test failed: {}", err.unwrap_or_default());
            }
        }
    }
    Ok(())
}

async fn run_model(args: ModelArgs) -> Result<()> {
    match args.command {
        None | Some(ModelCommand::List) => {
            let cfg = build_agent_config(None)?;
            xai_grok_pager::models::list_available_models(&cfg).await?;
        }
        Some(ModelCommand::Switch { model }) => {
            let id = model.strip_prefix("omgb-").unwrap_or(&model).to_string();
            providers::set_default_provider(&id)?;
            println!("default model switched to omgb-{id}");
        }
    }
    Ok(())
}

async fn run_cron(args: CronArgs) -> Result<()> {
    scheduler::add_job(
        args.name,
        &args.expression,
        &args.prompt,
        args.model,
        args.yolo,
    )
    .await
}

async fn run_schedule(args: ScheduleArgs) -> Result<()> {
    use scheduler::*;
    match args.command {
        ScheduleCommand::List => list_jobs().await,
        ScheduleCommand::Add(cron) => {
            add_job(
                cron.name,
                &cron.expression,
                &cron.prompt,
                cron.model,
                cron.yolo,
            )
            .await
        }
        ScheduleCommand::Delete { name } => delete_job(&name).await,
        ScheduleCommand::Run { name } => run_job(&name, false).await,
        ScheduleCommand::Start => spawn_daemon().await,
        ScheduleCommand::Daemon => run_daemon_loop().await,
        ScheduleCommand::Stop => stop_daemon(),
    }
}

async fn run_team(args: TeamArgs) -> Result<()> {
    if !args.yolo {
        bail!("`omgb team` requires --yolo to auto-approve tool use");
    }
    let git = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    if !git.success() {
        bail!("team mode requires a git repository");
    }

    let mut tasks = Vec::new();
    for i in 0..args.agents {
        let prompt = format!(
            "You are agent {}/{total}. {prompt}\n\nFocus on your slice and avoid duplicating other agents.\n\nWrite your changes to files in the repository.",
            i + 1,
            total = args.agents,
            prompt = args.prompt
        );
        let model = args.model.clone();
        let yolo = args.yolo;
        let worktree = std::env::temp_dir().join(format!("omgb-team-{i}-{}", uuid::Uuid::new_v4()));

        tasks.push(async move {
            let branch = create_worktree(&worktree).await?;
            run_single_turn_with(
                &prompt,
                model,
                yolo,
                OutputFormat::Plain,
                None,
                None,
                None,
                None,
                Some(worktree.clone()),
                &SessionParams::default(),
                false,
            )
            .await?;
            Ok::<_, anyhow::Error>((worktree, branch))
        });
    }

    let mut worktrees = Vec::new();
    for result in futures::future::join_all(tasks).await {
        match result {
            Ok(w) => worktrees.push(w),
            Err(e) => eprintln!("agent failed: {e}"),
        }
    }
    if worktrees.is_empty() {
        bail!("all team agents failed");
    }

    let mut failed_merges = Vec::new();
    for (w, branch) in &worktrees {
        if let Err(e) = merge_worktree_into_main(w, branch).await {
            eprintln!(
                "warning: failed to merge worktree {}: {e}; leaving it for manual resolution",
                w.display()
            );
            failed_merges.push(w.clone());
            continue;
        }
        if let Err(e) = remove_worktree(w).await {
            eprintln!("warning: failed to remove worktree {}: {e}", w.display());
        }
    }

    if failed_merges.len() == worktrees.len() {
        bail!("failed to merge any worktree; see warnings above");
    }
    if failed_merges.is_empty() {
        println!(
            "merged changes from all {} agent(s) into the working tree",
            worktrees.len()
        );
    } else {
        println!(
            "merged changes from {} agent(s); {} worktree(s) left for manual merge",
            worktrees.len() - failed_merges.len(),
            failed_merges.len()
        );
    }
    Ok(())
}

async fn create_worktree(path: &PathBuf) -> Result<String> {
    let branch = format!("omgb-team-{}", uuid::Uuid::new_v4());
    let out = Command::new("git")
        .args(["worktree", "add", "-b", &branch, "-q"])
        .arg(path)
        .output()
        .await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git worktree add failed: {stderr}");
    }
    Ok(branch)
}

async fn remove_worktree(path: &PathBuf) -> Result<()> {
    let out = Command::new("git")
        .args(["worktree", "remove", "--force", "-q"])
        .arg(path)
        .output()
        .await;
    if let Err(e) = out {
        eprintln!(
            "warning: failed to run git worktree remove for {}: {e}",
            path.display()
        );
    }
    if let Err(e) = tokio::fs::remove_dir_all(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!(
            "warning: failed to remove worktree directory {}: {e}",
            path.display()
        );
    }
    Ok(())
}

async fn merge_worktree_into_main(worktree: &PathBuf, branch: &str) -> Result<()> {
    let stage = Command::new("git")
        .current_dir(worktree)
        .args(["add", "-A"])
        .status()
        .await?;
    if !stage.success() {
        bail!("git add -A in worktree failed");
    }

    let diff = Command::new("git")
        .current_dir(worktree)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .await?;
    if diff.success() {
        return Ok(());
    }

    let (name, email) = git_author().await?;
    let commit_msg = format!("omgb team agent {branch}");
    let commit = Command::new("git")
        .current_dir(worktree)
        .env("GIT_AUTHOR_NAME", &name)
        .env("GIT_AUTHOR_EMAIL", &email)
        .env("GIT_COMMITTER_NAME", &name)
        .env("GIT_COMMITTER_EMAIL", &email)
        .args(["commit", "-m", commit_msg.as_str(), "--no-gpg-sign"])
        .status()
        .await?;
    if !commit.success() {
        bail!("git commit in worktree failed");
    }

    let main_dir = git_repo_root().await?;
    let merge_msg = format!("Merge omgb team agent {branch}");
    let merge = Command::new("git")
        .current_dir(&main_dir)
        .args([
            "merge",
            "--no-ff",
            "-m",
            merge_msg.as_str(),
            "--no-gpg-sign",
            branch,
        ])
        .status()
        .await?;
    if !merge.success() {
        bail!("git merge failed");
    }
    let delete = Command::new("git")
        .args(["branch", "-D", branch])
        .status()
        .await;
    if let Err(e) = delete {
        eprintln!("warning: failed to delete team branch {branch}: {e}");
    } else if let Ok(status) = delete
        && !status.success()
    {
        eprintln!(
            "warning: failed to delete team branch {branch}: exit {}",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

async fn run_swarm(args: SwarmArgs) -> Result<()> {
    if !args.yolo {
        bail!("`omgb swarm` requires --yolo to auto-approve tool use");
    }
    let result = if args.ensemble {
        swarm::run_swarm_ensemble(&args.prompt, args.model, args.yolo, args.count).await?
    } else {
        swarm::run_swarm_task_splitting(&args.prompt, args.model, args.yolo, args.count).await?
    };
    println!("{result}");
    Ok(())
}

async fn run_subagent(args: SubagentArgs) -> Result<()> {
    match args.command {
        SubagentCommand::Spawn { prompt, yolo } => subagents::spawn(&prompt, yolo).await,
        SubagentCommand::List => subagents::list(),
        SubagentCommand::Kill { id } => subagents::kill(&id),
        SubagentCommand::Logs { id } => subagents::logs(&id).await,
        SubagentCommand::Trace { id } => subagents::trace(&id).await,
    }
}

async fn run_harness(args: HarnessArgs) -> Result<()> {
    match args.command {
        HarnessCommand::Add {
            name,
            r#type,
            command,
            url,
            cwd,
            secret_env_key,
            allow_local,
            allow_private,
        } => {
            harness::add_connector(
                name,
                r#type,
                command,
                url,
                cwd,
                secret_env_key,
                allow_local,
                allow_private,
            )?;
        }
        HarnessCommand::List => {
            for c in harness::list_connectors()? {
                println!(
                    "{} ({}) command={:?} url={:?}",
                    c.name, c.r#type, c.command, c.url
                );
            }
        }
        HarnessCommand::Remove { name } => {
            harness::remove_connector(&name)?;
        }
        HarnessCommand::Run { name, prompt } => {
            harness::run_connector(&name, &prompt).await?;
        }
    }
    Ok(())
}

fn run_taste(args: TasteArgs) -> Result<()> {
    match args.command {
        TasteCommand::Like { note } => {
            taste::add_like(&note)?;
            println!("recorded like");
        }
        TasteCommand::Dislike { note } => {
            taste::add_dislike(&note)?;
            println!("recorded dislike");
        }
        TasteCommand::List => taste::list_taste()?,
    }
    Ok(())
}
