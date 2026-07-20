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
mod net;
mod providers;
mod research;
mod scheduler;
mod server;
mod subagents;
mod taste;
mod timeline;

use args::*;

fn desktop_control_allowed() -> bool {
    std::env::var("OMGB_ALLOW_DESKTOP_CONTROL")
        .is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
}

/// Loads `*_API_KEY` entries referenced by configured providers/connectors
/// from `~/.omgb/.env` into the process environment.
///
/// This is a bridge to the upstream Grok Build harness, which reads provider
/// secrets from the process environment via `env_key`. Only the keys that are
/// actually referenced are loaded, and only before any other thread starts.
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
    // Load referenced API keys from ~/.omgb/.env before anything else can read
    // the process environment. This is safe because it is the very first
    // operation and runs before any other thread or signal handler is installed.
    // SAFETY: no other threads exist at this point.
    unsafe { load_omg_env_into_process() }?;

    let cli = OmgbArgs::parse();

    if let Some(OmgbCommand::Autonomous(ref args)) = cli.command.as_ref() {
        let cwd = std::env::current_dir()?;
        xai_grok_shell::config::apply_sandbox(None, Some(&args.sandbox_profile), Some(&cwd));
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
            research::run_research(&args.topic, args.count, args.model, args.output).await
        }
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
    let mut argv = vec!["grok".to_string()];
    if let Some(m) = args.model {
        argv.push("--model".to_string());
        argv.push(m);
    }
    if let Some(p) = args.prompt {
        argv.push("--".to_string());
        argv.push(p);
    }
    let pager_args = PagerArgs::parse_from(argv);
    pager_run(pager_args, None).await?;
    Ok(())
}

async fn resolve_exec_prompt(args: &ExecArgs) -> Result<String> {
    if let Some(path) = &args.prompt_file {
        return tokio::fs::read_to_string(path)
            .await
            .map_err(|e| anyhow::anyhow!("failed to read prompt file: {e}"));
    }
    if let Some(p) = &args.prompt {
        return Ok(p.clone());
    }
    bail!("prompt is required")
}

pub(crate) async fn write_prompt_temp(prompt: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("omgb-prompt-{}.txt", uuid::Uuid::new_v4()));
    tokio::fs::write(&path, prompt.as_bytes()).await?;
    Ok(path)
}

async fn run_exec(args: ExecArgs) -> Result<()> {
    let prompt = resolve_exec_prompt(&args).await?;
    if let Some(path) = &args.output_file {
        let (prompt_file, own_prompt) = if let Some(p) = &args.prompt_file {
            (p.clone(), false)
        } else {
            (write_prompt_temp(&prompt).await?, true)
        };
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("exec")
            .arg("--prompt-file")
            .arg(&prompt_file)
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
        let out = cmd.output().await?;
        if own_prompt {
            let _ = tokio::fs::remove_file(&prompt_file).await;
        }
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("exec failed: {stderr}");
        }
        std::fs::write(path, &out.stdout)?;
        println!("wrote output to {}", path.display());
        if args.commit || args.commit_untracked {
            git_commit_all("omgb exec", args.commit_untracked, Some(path.as_path())).await?;
        }
        return Ok(());
    }

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
        None,
        None,
    )
    .await?;
    if args.commit || args.commit_untracked {
        git_commit_all("omgb exec", args.commit_untracked, None).await?;
    }
    Ok(())
}

async fn run_autonomous(args: AutonomousArgs) -> Result<()> {
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
        true,
        OutputFormat::Plain,
        Some(50),
        Some("bash,edit,file_search,read,write".to_string()),
        None,
    )
    .await
}

async fn run_use(args: UseArgs) -> Result<()> {
    if !(args.yolo || desktop_control_allowed()) {
        bail!("desktop control requires --yolo or OMGB_ALLOW_DESKTOP_CONTROL=1/true/yes/on");
    }
    let prompt = format!("{}\n\nUse the computer as needed.", args.prompt);
    run_single_turn_with(
        &prompt,
        args.model,
        args.yolo,
        OutputFormat::Plain,
        None,
        Some("computer,read,write,bash".to_string()),
        None,
    )
    .await
}

async fn run_browser(args: BrowserArgs) -> Result<()> {
    if !(args.yolo || desktop_control_allowed()) {
        bail!("desktop control requires --yolo or OMGB_ALLOW_DESKTOP_CONTROL=1/true/yes/on");
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
        Some("browser,computer,read,write,bash".to_string()),
        None,
    )
    .await
}

async fn run_single_turn_with(
    prompt: &str,
    model: Option<String>,
    yolo: bool,
    output_format: OutputFormat,
    max_turns: Option<u32>,
    cli_tools: Option<String>,
    cwd: Option<PathBuf>,
) -> Result<()> {
    let full_prompt = format!("{}{}", prompt, taste::taste_preamble());
    let options = HeadlessOptions {
        session_id: None,
        resume: None,
        cwd,
        yolo,
        trust: yolo,
        output_format,
        json_schema: None,
        model,
        rules: None,
        system_prompt_override: None,
        continue_last_session: false,
        fork_session: false,
        worktree: None,
        restore_code: false,
        agent: None,
        agents_json: None,
        cli_tools,
        cli_disallowed_tools: None,
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
    ) {
        if !name.is_empty() && !email.is_empty() {
            return Ok((name, email));
        }
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
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", email)
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
    if !git_worktree_status().await?.0 {
        bail!("git working tree is not clean; commit or stash changes before running `omgb loop`");
    }

    let mut iteration = 0;
    let mut prompt = args.prompt.clone();
    let mut clean = true;
    let mut status = String::new();
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
        )
        .await?;

        (clean, status) = git_worktree_status().await?;
        if clean {
            println!("worktree clean; stopping loop.");
            break;
        }
        let mut diff = git_diff_text().await?;
        if diff.len() > 16_384 {
            let mut end = 16_384;
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

        tasks.push(tokio::spawn(async move {
            let branch = create_worktree(&worktree).await?;
            run_single_turn_with(
                &prompt,
                model,
                yolo,
                OutputFormat::Plain,
                None,
                None,
                Some(worktree.clone()),
            )
            .await?;
            Ok::<_, anyhow::Error>((worktree, branch))
        }));
    }

    let mut worktrees = Vec::new();
    for t in tasks {
        match t.await? {
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
    if let Err(e) = tokio::fs::remove_dir_all(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "warning: failed to remove worktree directory {}: {e}",
                path.display()
            );
        }
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
    let _ = Command::new("git")
        .args(["branch", "-D", branch])
        .status()
        .await;
    Ok(())
}

async fn run_swarm(args: SwarmArgs) -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut tasks = Vec::new();
    for i in 0..args.count {
        let prompt = format!(
            "Swarm member {}/{total}: {prompt}\n\nProvide a concise answer.",
            i + 1,
            total = args.count,
            prompt = args.prompt
        );
        let model = args.model.clone();
        let yolo = args.yolo;
        let output_file =
            std::env::temp_dir().join(format!("omgb-swarm-{i}-{}.txt", uuid::Uuid::new_v4()));

        tasks.push(tokio::spawn(async move {
            let prompt_file = write_prompt_temp(&prompt).await?;
            let mut cmd = Command::new(&exe);
            cmd.arg("exec")
                .arg("--output-file")
                .arg(&output_file)
                .arg("--prompt-file")
                .arg(&prompt_file)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit());
            if let Some(m) = &model {
                cmd.arg("--model").arg(m);
            }
            if yolo {
                cmd.arg("--yolo");
            }

            let status = cmd.status().await?;
            let _ = tokio::fs::remove_file(&prompt_file).await;
            if !status.success() {
                bail!("swarm member {} failed", i + 1);
            }
            let text = tokio::fs::read_to_string(&output_file).await?;
            let _ = tokio::fs::remove_file(&output_file).await;
            Ok::<_, anyhow::Error>(text)
        }));
    }

    let mut outputs = Vec::new();
    let mut failed = 0usize;
    for t in tasks {
        match t.await? {
            Ok(text) => outputs.push(text),
            Err(e) => {
                eprintln!("{e}");
                failed += 1;
            }
        }
    }
    if outputs.is_empty() {
        bail!("all swarm members failed");
    }

    let winner = swarm_vote(&outputs);
    println!("{winner}");
    if failed > 0 {
        eprintln!("warning: {failed} swarm member(s) failed; winner chosen from remaining outputs");
    }
    Ok(())
}

fn swarm_vote(outputs: &[String]) -> String {
    let mut best = String::new();
    let mut best_count = 0usize;
    let mut best_index = usize::MAX;
    let mut seen: std::collections::HashMap<String, (usize, usize, String)> =
        std::collections::HashMap::new();

    for (i, o) in outputs.iter().enumerate() {
        let key = o.trim().to_string();
        let entry = seen.entry(key).or_insert_with(|| (0, i, o.clone()));
        entry.0 += 1;

        let (count, idx, original) = (entry.0, entry.1, &entry.2);
        if count > best_count || (count == best_count && idx < best_index) {
            best_count = count;
            best_index = idx;
            best = original.clone();
        }
    }
    best
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
            api_key,
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
                api_key,
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
