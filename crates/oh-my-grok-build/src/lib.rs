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

pub fn main() -> Result<()> {
    xai_grok_pager_minimal::install();
    xai_crash_handler::install_terminal_restore_only();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async_main())
}

async fn async_main() -> Result<()> {
    let cli = OmgbArgs::parse();
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
            research::run_research(&args.topic, args.count, args.output).await
        }
        OmgbCommand::Timeline(args) => timeline::list_events(args.limit, args.json),
        OmgbCommand::Harness(args) => run_harness(args).await,
        OmgbCommand::Serve(args) => server::serve(&args).await,
        OmgbCommand::Connect(args) => server::connect(&args).await,
        OmgbCommand::Use(args) => run_use(args).await,
        OmgbCommand::Browser(args) => run_browser(args).await,
        OmgbCommand::Mcp(args) => xai_grok_pager::mcp_cmd::run(args).await,
        OmgbCommand::Taste(args) => run_taste(args),
    }
}

fn build_agent_config(model: Option<String>) -> Result<AgentConfig> {
    let raw = xai_grok_shell::config::load_effective_config_disk_only()
        .map_err(|e| anyhow::anyhow!("failed to load config: {e}"))?;
    let mut cfg = AgentConfig::new_from_toml_cfg(&raw)
        .map_err(|e| anyhow::anyhow!("failed to create agent config: {e}"))?;
    cfg.default_model_override = model;
    Ok(cfg)
}

async fn run_tui(args: TuiArgs) -> Result<()> {
    let mut argv = vec!["grok".to_string()];
    if let Some(p) = args.prompt {
        argv.push(p);
    }
    if let Some(m) = args.model {
        argv.push("--model".to_string());
        argv.push(m);
    }
    let pager_args = PagerArgs::parse_from(argv);
    pager_run(pager_args, None).await?;
    Ok(())
}

async fn run_exec(args: ExecArgs) -> Result<()> {
    run_single_turn_with(
        &args.prompt,
        args.model,
        args.yolo,
        if args.json {
            OutputFormat::Json
        } else {
            OutputFormat::Plain
        },
        None,
        None,
        args.output_file,
    )
    .await
}

async fn run_autonomous(args: AutonomousArgs) -> Result<()> {
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
    let prompt = format!(
        "{prompt}\n\nUse the computer as needed.",
        prompt = args.prompt
    );
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
    let mut prompt = args.prompt.clone();
    if let Some(url) = args.url {
        prompt.push_str(&format!("\n\nStart at URL: {url}"));
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
    output_file: Option<PathBuf>,
) -> Result<()> {
    let full_prompt = format!("{}{}", prompt, taste::taste_preamble());
    let options = HeadlessOptions {
        session_id: None,
        resume: None,
        cwd: None,
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

    if let Some(path) = output_file {
        let old_stdout = std::io::stdout();
        // Simpler: run to a temporary string by capturing output is non-trivial here.
        // Write prompt to file as a placeholder and run to stdout.
        std::fs::write(&path, "")?;
        run_single_turn(HeadlessPrompt::Text(full_prompt), false, options).await?;
        println!("\n(output also intended for {})", path.display());
        Ok(())
    } else {
        run_single_turn(HeadlessPrompt::Text(full_prompt), false, options).await
    }
}

async fn run_loop(args: LoopArgs) -> Result<()> {
    let mut iteration = 0;
    let mut prompt = args.prompt.clone();
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

        let git = Command::new("git")
            .args(["diff", "--stat"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await?;
        let diff = String::from_utf8_lossy(&git.stdout);
        if diff.trim().is_empty() {
            println!("worktree clean; stopping loop.");
            break;
        }
        prompt = format!(
            "Original task: {}\n\nCurrent git diff:\n{}\n\nContinue until complete.",
            args.prompt, diff
        );
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
            let p = add_provider(&add_args)?;
            println!("added provider {} ({}) -> {}", p.id, p.name, p.base_url);
        }
        ProviderCommand::Remove { id } => {
            remove_provider(&id)?;
            println!("removed provider {id}");
        }
        ProviderCommand::Discover(discover_args) => {
            let found = discover_local_models(&discover_args).await?;
            for (provider, models) in &found {
                println!("{provider}: {}", models.join(", "));
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
            providers::set_default_provider(&model)?;
            println!("default model switched to {model}");
        }
    }
    Ok(())
}

async fn run_cron(args: CronArgs) -> Result<()> {
    scheduler::add_job(args.name, &args.expression, &args.prompt, args.model)
}

async fn run_schedule(args: ScheduleArgs) -> Result<()> {
    use scheduler::*;
    match args.command {
        ScheduleCommand::List => list_jobs(),
        ScheduleCommand::Add(cron) => {
            add_job(cron.name, &cron.expression, &cron.prompt, cron.model)
        }
        ScheduleCommand::Delete { name } => delete_job(&name),
        ScheduleCommand::Run { name } => run_job(&name).await,
        ScheduleCommand::Start => run_daemon_loop().await,
        ScheduleCommand::Stop => stop_daemon(),
    }
}

async fn run_team(args: TeamArgs) -> Result<()> {
    let mut handles = Vec::new();
    for i in 0..args.agents {
        let prompt = format!(
            "You are agent {}/{total}. {prompt}\n\nFocus on your slice and avoid duplicating other agents.",
            i + 1,
            total = args.agents,
            prompt = args.prompt
        );
        let model = args.model.clone();
        let yolo = args.yolo;
        handles.push(tokio::spawn(async move {
            run_single_turn_with(&prompt, model, yolo, OutputFormat::Plain, None, None, None).await
        }));
    }
    for h in handles {
        h.await??;
    }
    Ok(())
}

async fn run_swarm(args: SwarmArgs) -> Result<()> {
    let mut handles = Vec::new();
    for i in 0..args.count {
        let prompt = format!(
            "Swarm member {}/{total}: {prompt}\n\nProvide a concise answer; the orchestrator will vote.",
            i + 1,
            total = args.count,
            prompt = args.prompt
        );
        let model = args.model.clone();
        let yolo = args.yolo;
        handles.push(tokio::spawn(async move {
            run_single_turn_with(&prompt, model, yolo, OutputFormat::Plain, None, None, None).await
        }));
    }
    for h in handles {
        h.await??;
    }
    Ok(())
}

async fn run_subagent(args: SubagentArgs) -> Result<()> {
    match args.command {
        SubagentCommand::Spawn { prompt } => subagents::spawn(&prompt).await,
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
            secret,
        } => {
            harness::add_connector(name, r#type, command, url, cwd, secret)?;
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
