//! `oh-my-grok-build` / `omgb` composition-root binary.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::OnceLock;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use tokio::io::AsyncWrite;
use tokio::process::Command;

use xai_grok_pager::app::{PagerArgs, run as pager_run};
use xai_grok_pager::headless::{HeadlessOptions, HeadlessPrompt, OutputFormat, run_single_turn};
use xai_grok_shell::agent::config::Config as AgentConfig;

mod args;
mod auth;
mod doctor;
mod group;
mod harness;
mod hashline;
mod lsp;
mod marketplace;
mod memory;
mod meta;
mod moe;
mod net;
mod notifications;
mod playbook;
mod pr;
mod prompt_context;
mod prompt_guard;
mod providers;
mod research;
mod scheduler;
mod server;
mod session;
mod skill;
mod subagents;
mod swarm;
mod taste;
mod threads;
mod timeline;
mod tool_overrides;
mod tools;
mod update;
#[cfg(windows)]
mod win_sid;
mod workflow;

use args::*;

#[cfg(test)]
pub(crate) static OMGB_HOME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn desktop_control_allowed() -> bool {
    std::env::var("OMGB_ALLOW_DESKTOP_CONTROL")
        .is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
}

fn set_default_env(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: `main()` is single-threaded at this point; this runs before the
        // Tokio runtime or any signal handlers are installed, so no other thread
        // can be reading or writing the environment.
        unsafe { std::env::set_var(key, value) };
    }
}

/// Loads `*_API_KEY` entries from `~/.omgb/.env` into the process environment,
/// but only for keys referenced by configured providers, connectors, known
/// catalog templates, or built-in web search integrations. This limits the
/// secrets visible to child processes while still letting upstream Grok Build
/// resolve `env_key` references.
///
/// Existing process environment values are preserved (env overrides dotenv).
///
/// # Safety
/// Must be called before any other thread can read or write the environment.
/// This is the first operation in `main()`, before the Tokio runtime or any
/// signal handlers are installed.
unsafe fn load_omg_env_into_process() -> Result<()> {
    // Best-effort: if ~/.omgb/.env cannot be read yet (e.g. OMGB_HOME is not
    // set and the home directory is unknown), skip the bridge. Commands that
    // actually need secrets will report the missing key when they try to read
    // the file.
    if let Ok(dotenv) = crate::providers::load_env_file() {
        let allowed = crate::providers::env_keys_to_load();
        for (k, v) in dotenv {
            // OMGB_API_KEY is a transient input for `omgb provider add`; it is not
            // an upstream env_key and should not be exposed to the whole process.
            if k == "OMGB_API_KEY" {
                continue;
            }
            if allowed.contains(&k)
                && crate::providers::is_valid_env_key(&k)
                && !v.is_empty()
                && std::env::var(&k).ok().filter(|v| !v.is_empty()).is_none()
            {
                // SAFETY: see the function-level safety contract above.
                unsafe { std::env::set_var(k, v) };
            }
        }
    }
    Ok(())
}

/// Provision the bounded local tool-action ledger before the runtime starts.
/// The exact path is owned by omgb; a caller-provided environment override is
/// deliberately ignored so tool arguments cannot be redirected to an
/// attacker-controlled audit sink.
fn configure_tool_audit() -> Result<()> {
    let root = crate::providers::omg_dir()?;
    std::fs::create_dir_all(&root)?;
    crate::providers::restrict_omg_directory_permissions(&root)?;
    let path = root.join("tool_actions.jsonl");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            crate::providers::restrict_omg_file_permissions(&path)?;
        }
        Ok(_) => bail!(
            "tool action audit must be a regular file: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::providers::write_file_atomic(&path, String::new(), true)?;
        }
        Err(error) => return Err(error.into()),
    }
    // SAFETY: main is still single-threaded; this runs before Tokio, signal
    // handlers, or the tool registry starts.
    unsafe { std::env::set_var("OMGB_TOOL_AUDIT_PATH", &path) };
    Ok(())
}

pub fn main() -> Result<()> {
    // Load referenced BYOK keys from ~/.omgb/.env into the process environment
    // before any other thread can observe it. Upstream Grok Build resolves
    // env_key via std::env::var, so this bridge is required.
    // SAFETY: no other threads exist; this is the very first operation.
    unsafe { load_omg_env_into_process() }?;
    configure_tool_audit()?;

    // omgb: opt out of upstream telemetry/feedback by default. Users can opt in
    // by setting these env vars or [features] flags in ~/.grok/config.toml.
    set_default_env("GROK_TELEMETRY_ENABLED", "false");
    set_default_env("GROK_FEEDBACK_ENABLED", "false");
    set_default_env("GROK_ERROR_REPORTING", "false");

    // omgb: enable upstream slash features that are otherwise gated off by default.
    set_default_env("GROK_VOICE_MODE", "true");
    set_default_env("GROK_SESSION_RECAP", "true");
    set_default_env("GROK_MEMORY", "1");

    xai_grok_tools::registry::types::register_tool_pack(crate::tools::register);

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

static GIT_EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
static SAFE_PATH: OnceLock<OsString> = OnceLock::new();

fn git_executable_path() -> Option<&'static PathBuf> {
    GIT_EXE
        .get_or_init(|| {
            let exe = if cfg!(windows) { "git.exe" } else { "git" };
            std::env::var_os("PATH").and_then(|path_var| {
                std::env::split_paths(&path_var)
                    .map(|d| d.join(exe))
                    .find(|p| p.is_file())
            })
        })
        .as_ref()
}

fn safe_path() -> &'static OsString {
    SAFE_PATH.get_or_init(|| {
        let mut dirs = Vec::new();
        if let Some(git) = git_executable_path()
            && let Some(parent) = git.parent()
        {
            dirs.push(parent.to_path_buf());
            if let Some(gp) = parent.parent() {
                for sub in &[
                    "bin",
                    "usr/bin",
                    "mingw64/bin",
                    "libexec/git-core",
                    "usr/libexec/git-core",
                    "mingw64/libexec/git-core",
                ] {
                    dirs.push(gp.join(sub));
                }
            }
        }
        if cfg!(windows) {
            if let Ok(windir) = std::env::var("SystemRoot") {
                dirs.push(PathBuf::from(&windir).join("system32"));
                dirs.push(PathBuf::from(windir));
            }
        } else {
            for d in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
                dirs.push(PathBuf::from(d));
            }
        }
        let seen: HashSet<_> = dirs.into_iter().filter(|p| p.is_dir()).collect();
        std::env::join_paths(seen).unwrap_or_else(|_| OsString::new())
    })
}

fn git_exec_path() -> Option<PathBuf> {
    git_executable_path().and_then(|git| {
        for anc in git.ancestors() {
            for sub in &[
                "libexec/git-core",
                "mingw64/libexec/git-core",
                "usr/libexec/git-core",
            ] {
                let candidate = anc.join(sub);
                if candidate.is_dir() {
                    return Some(candidate);
                }
            }
        }
        None
    })
}

/// Returns a `git` command with a sanitized environment: a minimal PATH, no
/// inherited `GIT_*` or `LD_*` variables, and pager/editor/askpass disabled.
pub(crate) fn git_cmd() -> Command {
    let mut cmd = if let Some(git) = git_executable_path() {
        Command::new(git)
    } else {
        Command::new("git")
    };
    cmd.env_clear();
    if let Some(home) = dirs::home_dir() {
        cmd.env("HOME", &home);
        cmd.env("USERPROFILE", &home);
    }
    if let Some(exec_path) = git_exec_path() {
        cmd.env("GIT_EXEC_PATH", exec_path);
    }
    cmd.env("PATH", safe_path())
        .env("GIT_PAGER", "cat")
        .env("GIT_EDITOR", "")
        .env("GIT_SEQUENCE_EDITOR", "")
        .env("GIT_ASKPASS", "")
        .env("GIT_TERMINAL_PROMPT", "0");
    cmd
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
        OmgbCommand::Auth(args) => auth::run_auth(args).await,
        OmgbCommand::Model(args) => run_model(args).await,
        OmgbCommand::Cron(args) => run_cron(args).await,
        OmgbCommand::Schedule(args) => run_schedule(args).await,
        OmgbCommand::Team(args) => run_team(args).await,
        OmgbCommand::Swarm(args) => run_swarm(args).await,
        OmgbCommand::Subagent(args) => run_subagent(args).await,
        OmgbCommand::Thread(args) => threads::run_thread(args).await,
        OmgbCommand::Meta(args) => meta::run_meta(args).await,
        OmgbCommand::Research(args) => {
            research::run_research(&args.topic, args.count, args.model, args.yolo, args.output)
                .await
        }
        OmgbCommand::Session(args) => session::run_session(args).await,
        OmgbCommand::Memory(args) => memory::run_memory(args),
        OmgbCommand::Hashline(args) => hashline::run_hashline(args),
        OmgbCommand::Pr(args) => pr::run_pr(args).await,
        OmgbCommand::Lsp(args) => lsp::run_lsp(args).await,
        OmgbCommand::Dap(args) => lsp::run_dap(args).await,
        OmgbCommand::Plugin(args) => marketplace::run_plugin(args).await,
        OmgbCommand::Playbook(args) => playbook::run_playbook(&args).await,
        OmgbCommand::Workflow(args) => workflow::run_workflow(&args).await,
        OmgbCommand::Timeline(args) => timeline::list_events(args.limit, args.json),
        OmgbCommand::Group(args) => group::run_group(&args).await,
        OmgbCommand::Harness(args) => run_harness(args).await,
        OmgbCommand::Serve(args) => server::serve(&args).await,
        OmgbCommand::Connect(args) => server::connect(&args).await,
        OmgbCommand::Use(args) => run_use(args).await,
        OmgbCommand::Browser(args) => run_browser(args).await,
        OmgbCommand::Mcp(args) => xai_grok_pager::mcp_cmd::run(args).await,
        OmgbCommand::Doctor(args) => doctor::run_doctor(args.fix, args.json).await,
        OmgbCommand::Taste(args) => run_taste(args),
        OmgbCommand::Skill(args) => run_skill(args).await,
        OmgbCommand::Commit(args) => run_commit(args).await,
        OmgbCommand::Review => run_review().await,
        OmgbCommand::Undo(args) => run_undo(args).await,
        OmgbCommand::Feedback(args) => run_feedback(args).await,
        OmgbCommand::Update(args) => update::run(&args).await,
    }
}

pub(crate) fn build_agent_config(model: Option<String>) -> Result<AgentConfig> {
    let mut raw = xai_grok_shell::config::load_effective_config_disk_only()
        .map_err(|e| anyhow::anyhow!("failed to load config: {e}"))?;
    // omgb: force-disable upstream telemetry and feedback by default. Feedback
    // is still accepted via `omgb feedback` which opens a GitHub issue.
    if let toml::Value::Table(ref mut t) = raw {
        let features = t
            .entry("features")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        if let toml::Value::Table(ft) = features {
            ft.insert("feedback".into(), toml::Value::Boolean(false));
            ft.insert("telemetry".into(), toml::Value::Boolean(false));
        }
    }
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

fn cache_provider_id(model: &str) -> String {
    model
        .trim()
        .strip_prefix("omgb-")
        .map(str::to_string)
        .or_else(|| crate::providers::resolve_model_to_provider(model))
        .unwrap_or_else(|| "grok-native".to_string())
}

fn pager_cache_policy(args: &PagerArgs) -> crate::prompt_context::WorkspacePolicy {
    crate::prompt_context::WorkspacePolicy {
        sandbox_profile: config_sandbox_profile(),
        yolo: args.yolo,
        trust: args.trust,
        permission_mode: args.permission_mode_flag.clone(),
        cli_tools: args.cli_tools.clone(),
        cli_disallowed_tools: args.cli_disallowed_tools.clone(),
        allow_rules: args.allow_rules.clone(),
        deny_rules: args.deny_rules.clone(),
        disable_web_search: args.disable_web_search,
        agent: args.agent.clone(),
        agent_manifest_sha256: crate::prompt_context::json_content_sha256(
            args.agents_json.as_deref(),
        ),
        reasoning_effort: args.reasoning_effort.clone(),
    }
}

fn headless_cache_policy(options: &HeadlessOptions) -> crate::prompt_context::WorkspacePolicy {
    crate::prompt_context::WorkspacePolicy {
        sandbox_profile: config_sandbox_profile(),
        yolo: options.yolo,
        trust: options.trust,
        permission_mode: options.permission_mode_flag.clone(),
        cli_tools: options.cli_tools.clone(),
        cli_disallowed_tools: options.cli_disallowed_tools.clone(),
        allow_rules: options.allow_rules.clone(),
        deny_rules: options.deny_rules.clone(),
        disable_web_search: options.disable_web_search,
        agent: options.agent.clone(),
        agent_manifest_sha256: crate::prompt_context::json_content_sha256(
            options.agents_json.as_deref(),
        ),
        reasoning_effort: options.reasoning_effort.clone(),
    }
}

pub(crate) async fn run_tui(args: TuiArgs) -> Result<()> {
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
    let mut pager_args = PagerArgs::parse_from(argv);

    let overrides = crate::tool_overrides::load_tool_overrides(
        pager_args.agent.as_deref(),
        pager_args.session_id.as_deref(),
    )?;
    crate::tool_overrides::apply_tool_overrides_to_pager_args(&overrides, &mut pager_args)?;

    let context = crate::prompt_context::compile(
        vec![
            ("00-user-rules", pager_args.rules.take().unwrap_or_default()),
            ("10-skills", crate::skill::skill_preamble()),
            ("20-taste", crate::taste::taste_preamble()),
        ],
        vec![],
    );
    let cache_model = pager_args.model.clone().or_else(|| {
        build_agent_config(None)
            .ok()
            .and_then(|config| config.models.default)
    });
    if let Some(model) = cache_model.as_deref() {
        let provider_fingerprint = crate::providers::provider_execution_fingerprint(model)
            .ok()
            .flatten();
        if let Some(affinity) = crate::prompt_context::cache_affinity(
            &context,
            &cache_provider_id(model),
            model,
            provider_fingerprint.as_deref(),
            pager_cache_policy(&pager_args),
        ) {
            crate::prompt_context::record_cache_affinity(&context, &affinity, 1);
        }
    } else {
        crate::prompt_context::record_cache_shape(&context);
    }
    pager_args.rules = context.rules;

    pager_run(pager_args, None).await?;
    Ok(())
}

pub(crate) fn scratch_dir() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("tmp"))
}

/// Resolve a user-supplied path (for `--output-file` / `--prompt-file`) to an
/// absolute path that stays inside the current working directory or the omgb
/// scratch directory. Rejects `..` components, symlinks (including broken ones),
/// and directories. Symlinks are never followed: each existing component is
/// checked before traversal, and absolute paths are resolved component by
/// component from the filesystem root.
fn resolve_path(raw: &std::path::Path) -> Result<std::path::PathBuf> {
    let cwd = std::env::current_dir()?;
    let canonical_cwd =
        dunce::canonicalize(&cwd).unwrap_or_else(|_| dunce::simplified(&cwd).to_path_buf());
    if !canonical_cwd.is_dir() {
        bail!("cannot resolve current directory: {}", cwd.display());
    }

    let is_absolute = raw.is_absolute();
    let mut current = if is_absolute {
        std::path::PathBuf::new()
    } else {
        canonical_cwd.clone()
    };
    let mut saw_prefix = false;
    let mut saw_name = false;
    let mut components = raw.components().peekable();

    while let Some(comp) = components.next() {
        match comp {
            std::path::Component::Prefix(p) => {
                saw_prefix = true;
                match p.kind() {
                    std::path::Prefix::Disk(d) | std::path::Prefix::VerbatimDisk(d) => {
                        current = std::path::PathBuf::from(format!("{}:\\", d as char));
                    }
                    _ => {
                        bail!(
                            "path must not use network/verbatim prefixes: {}",
                            raw.display()
                        );
                    }
                }
            }
            std::path::Component::RootDir => {
                if current.as_os_str().is_empty() {
                    current = std::path::PathBuf::from("/");
                } else if cfg!(windows) && saw_prefix {
                    // Root dir after a Windows drive letter is already represented.
                } else {
                    bail!(
                        "path must be relative and not contain absolute components: {}",
                        raw.display()
                    );
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !is_absolute {
                    bail!("path must not contain '..' components: {}", raw.display());
                }
                if !current.pop() {
                    bail!("path must not escape the root directory: {}", raw.display());
                }
            }
            std::path::Component::Normal(name) => {
                saw_name = true;
                current.push(name);

                match std::fs::symlink_metadata(&current) {
                    Ok(meta) => {
                        if meta.is_symlink() {
                            bail!("path must not contain a symlink: {}", raw.display());
                        }
                        let is_last = components.peek().is_none();
                        if is_last {
                            if meta.is_dir() {
                                bail!("path must not be a directory: {}", raw.display());
                            }
                        } else if !meta.is_dir() {
                            bail!("path component is not a directory: {}", current.display());
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        // Missing intermediate directories or the final file are fine.
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }

    if !saw_name {
        bail!(
            "path must contain at least one file or directory name: {}",
            raw.display()
        );
    }

    if is_absolute {
        let scratch = scratch_dir()?;
        let canonical_scratch = dunce::canonicalize(&scratch).unwrap_or_else(|_| scratch.clone());
        if !current.starts_with(&canonical_cwd) && !current.starts_with(&canonical_scratch) {
            bail!(
                "path must be under the current working directory or omgb scratch directory: {}",
                raw.display()
            );
        }
    }

    Ok(current)
}

/// Read a prompt file without following a symlink (Unix: O_NOFOLLOW).
/// On Windows, this is a best-effort check because `std::fs` cannot open
/// without following reparse points.
#[cfg(unix)]
fn read_prompt_file(path: &std::path::Path) -> Result<String> {
    use std::ffi::CString;
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let cstr = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        anyhow::anyhow!("prompt file path contains a null byte: {}", path.display())
    })?;
    let fd = unsafe {
        libc::open(
            cstr.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        bail!("failed to open prompt file {}: {err}", path.display());
    }
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut s = String::new();
    f.read_to_string(&mut s)
        .with_context(|| format!("failed to read prompt file: {}", path.display()))?;
    Ok(s)
}

#[cfg(windows)]
fn read_prompt_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x00000400;

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .with_context(|| format!("failed to open prompt file: {}", path.display()))?;
    let meta = file
        .metadata()
        .with_context(|| format!("failed to stat prompt file: {}", path.display()))?;
    if meta.is_symlink() || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        bail!(
            "prompt file must not be a symlink or reparse point: {}",
            path.display()
        );
    }
    let mut s = String::new();
    file.read_to_string(&mut s)
        .with_context(|| format!("failed to read prompt file: {}", path.display()))?;
    Ok(s)
}

/// Write output to `path` without following a final symlink by writing to a
/// temporary file in the same directory and atomically renaming it into place.
fn write_output_file(path: &std::path::Path, contents: impl AsRef<[u8]>) -> Result<()> {
    crate::providers::write_file_atomic(path, contents, false)
}

pub(crate) async fn write_prompt_temp(prompt: &str) -> Result<PathBuf> {
    let dir = scratch_dir()?;
    let file_name = format!("omgb-prompt-{}.txt", uuid::Uuid::new_v4());
    let path = dir.join(&file_name);
    let path2 = path.clone();
    let prompt = prompt.as_bytes().to_vec();
    tokio::task::spawn_blocking(move || {
        crate::providers::restrict_omg_directory_permissions(&dir)?;
        crate::providers::write_file_atomic(&path2, prompt, true)
    })
    .await??;
    Ok(path)
}

pub(crate) struct PromptFileGuard(PathBuf);
impl PromptFileGuard {
    pub(crate) fn disarm(mut self) {
        self.0.clear();
    }
}
impl Drop for PromptFileGuard {
    fn drop(&mut self) {
        let path = std::mem::take(&mut self.0);
        if !path.as_os_str().is_empty() {
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
    let mut child = spawn_detached(cmd)?;
    let mut group = match xai_tty_utils::ProcessGroup::new() {
        Ok(group) => group,
        Err(error) => {
            let _ = child.start_kill();
            return Err(anyhow::anyhow!(
                "cannot safely contain child process tree: {error}"
            ));
        }
    };
    if let Err(error) = group.attach(&child) {
        let _ = child.start_kill();
        return Err(anyhow::anyhow!(
            "cannot safely attach child to its process group: {error}"
        ));
    }
    Ok((child, Some(group)))
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

/// AsyncWrite that keeps the first `limit` bytes and silently discards the rest.
/// This lets a child keep writing without filling the pipe and blocking.
pub(crate) struct BoundedCapture {
    buf: Vec<u8>,
    limit: usize,
}

impl BoundedCapture {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit,
        }
    }

    pub(crate) fn into_string(self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }
}

impl AsyncWrite for BoundedCapture {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let remaining = this.limit.saturating_sub(this.buf.len());
        let n = buf.len().min(remaining);
        if n > 0 {
            this.buf.extend_from_slice(&buf[..n]);
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn exe_stem() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default()
        .to_lowercase()
}

#[cfg(unix)]
pub(crate) fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let pid_s = pid.to_string();
    if !std::process::Command::new("kill")
        .args(["-0", &pid_s])
        .status()
        .is_ok_and(|s| s.success())
    {
        return false;
    }
    let exe = exe_stem();
    if exe.is_empty() {
        return false;
    }
    if let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) {
        let first = bytes.split(|b| *b == 0).next();
        if let Some(arg) = first {
            let arg = String::from_utf8_lossy(arg);
            if let Some(name) = std::path::Path::new(arg.as_ref()).file_stem() {
                let name = name.to_string_lossy().to_lowercase();
                if name == exe {
                    return true;
                }
            }
        }
    }
    if let Ok(output) = std::process::Command::new("ps")
        .args(["-p", &pid_s, "-o", "comm="])
        .output()
    {
        let comm = String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_lowercase();
        let comm_stem = std::path::Path::new(&comm)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        return comm_stem == exe;
    }
    false
}

#[cfg(windows)]
pub(crate) fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let exe = exe_stem();
    if exe.is_empty() {
        return false;
    }
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
        let Some(image) = parts.next() else {
            return false;
        };
        let Some(pid_field) = parts.next() else {
            return false;
        };
        let image_stem = std::path::Path::new(image.trim_matches('"'))
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        pid_field.trim_matches('"') == pid_s && image_stem == exe
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn process_alive(_pid: u32) -> bool {
    false
}

fn append_exec_common_args(cmd: &mut Command, args: &ExecArgs) {
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
    if let Some(n) = args.max_turns {
        cmd.arg("--max-turns").arg(n.to_string());
    }
    if args.memory {
        cmd.arg("--memory");
    }
    if let Some(r) = &args.session.resume {
        if r.is_empty() {
            cmd.arg("--resume");
        } else {
            cmd.arg("--resume").arg(r);
        }
    }
    if args.session.continue_last {
        cmd.arg("--continue");
    }
    if let Some(sid) = &args.session.session_id {
        cmd.arg("--session-id").arg(sid);
    }
    if args.session.fork_session {
        cmd.arg("--fork-session");
    }
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
        let p = p.clone();
        tokio::task::spawn_blocking(move || read_prompt_file(&p)).await??
    } else if let Some(p) = &args.prompt {
        p.clone()
    } else {
        bail!("prompt is required")
    };

    if let Some(path) = output_path {
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
        append_exec_common_args(&mut cmd, &args);
        let out = cmd.output().await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("exec failed: {stderr}");
        }
        let out_stdout = out.stdout;
        let output_path = path.clone();
        tokio::task::spawn_blocking(move || write_output_file(&output_path, &out_stdout)).await??;
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
        args.max_turns,
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
        args.max_turns.or(Some(50)),
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

pub(crate) async fn resolve_model_candidates(
    prompt: &str,
    explicit: Option<String>,
) -> Result<Vec<String>> {
    if let Some(m) = explicit {
        return Ok(vec![m]);
    }

    let mut candidates = Vec::new();
    if let Ok(id) = moe::select_provider_or_fallback(prompt).await
        && providers::ensure_provider_configured(&id).is_ok()
    {
        candidates.push(format!("omgb-{id}"));
    }

    let mut locals: Vec<String> = moe::available_providers()
        .await?
        .into_iter()
        .filter(|id| providers::is_local_provider_id(id))
        .collect();
    locals.sort_by(|a, b| {
        moe::provider_cost(a, None)
            .partial_cmp(&moe::provider_cost(b, None))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(b))
    });
    locals.dedup();
    for id in locals {
        let m = format!("omgb-{id}");
        if !candidates.contains(&m) {
            candidates.push(m);
        }
    }

    if let Ok(config) = build_agent_config(None)
        && let Some(default) = config.models.default
        && !candidates.contains(&default)
        && (!default.starts_with("omgb-")
            || providers::ensure_provider_configured(
                default.strip_prefix("omgb-").unwrap_or(&default),
            )
            .is_ok())
    {
        candidates.push(default);
    }

    if candidates.is_empty() {
        bail!(
            "no models available (configure BYOK/local first, or sign in and select a Grok model)"
        );
    }
    Ok(candidates)
}

fn session_id_for_model_attempt(
    auto_new_session: bool,
    explicit_session_id: Option<&str>,
) -> Option<String> {
    if auto_new_session {
        Some(uuid::Uuid::new_v4().to_string())
    } else {
        explicit_session_id.map(str::to_owned)
    }
}

fn may_retry_model_attempt(auto_new_session: bool, session_evidence_exists: bool) -> bool {
    auto_new_session && !session_evidence_exists
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
    run_single_turn_with_provider_fingerprint(
        prompt,
        model,
        yolo,
        output_format,
        max_turns,
        cli_tools,
        cli_disallowed_tools,
        agent,
        cwd,
        session,
        memory,
        None,
    )
    .await
}

async fn run_single_turn_with_provider_fingerprint(
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
    expected_provider_fingerprint: Option<String>,
) -> Result<()> {
    let candidates = resolve_model_candidates(prompt, model).await?;

    let mut volatile_rules = Vec::new();
    let mut one_shot_lease = None;
    if memory {
        let notes = crate::memory::recall(prompt, 5)?;
        one_shot_lease = crate::memory::lease_one_shot(prompt, 5)?;
        let shots = one_shot_lease
            .as_ref()
            .map(|lease| lease.notes())
            .unwrap_or_default();
        let recalled = crate::memory::format_prompt_memory(notes, shots);
        if !recalled.trim().is_empty() {
            volatile_rules.push(("90-memory", recalled));
        }
    }
    let context = crate::prompt_context::compile(
        vec![
            ("10-skills", crate::skill::skill_preamble()),
            ("20-taste", crate::taste::taste_preamble()),
        ],
        volatile_rules,
    );
    let rules = context.rules.clone();

    let resume = session.resume.as_ref().filter(|s| !s.is_empty()).cloned();
    let auto_new_session =
        resume.is_none() && !session.continue_last && session.session_id.is_none();
    let effective_session_id = session.session_id.clone();

    let mut options = HeadlessOptions {
        session_id: effective_session_id.clone(),
        resume: resume.clone(),
        cwd: cwd.clone(),
        yolo,
        trust: yolo,
        output_format,
        json_schema: None,
        model: None,
        rules,
        system_prompt_override: None,
        continue_last_session: session.continue_last,
        fork_session: session.fork_session,
        worktree: None,
        restore_code: false,
        agent: agent.clone(),
        agents_json: None,
        cli_tools,
        cli_disallowed_tools,
        disable_web_search: false,
        allow_rules: Vec::new(),
        deny_rules: Vec::new(),
        max_turns,
        permission_mode_flag: {
            let from_env = std::env::var("OMGB_PERMISSION_MODE")
                .ok()
                .filter(|s| !s.is_empty());
            if yolo {
                from_env
            } else {
                from_env.or(Some("auto".into()))
            }
        },
        reasoning_effort: None,
        self_verify: false,
        best_of_n: None,
        wait_for_background: true,
        background_wait_timeout: Duration::from_secs(300),
    };

    let overrides = crate::tool_overrides::load_tool_overrides(
        agent.as_deref(),
        effective_session_id.as_deref(),
    )?;
    crate::tool_overrides::apply_tool_overrides_to_headless_options(&overrides, &mut options)?;

    let cache_policy = headless_cache_policy(&options);
    let mut last_result: Result<()> = Err(anyhow::anyhow!("no usable model candidates"));
    let mut errors: Vec<String> = Vec::new();
    let mut last_attempt_session_id = effective_session_id.clone();
    let mut last_cache_affinity = None;
    for (attempt_index, m) in candidates.iter().enumerate() {
        let prepared_provider = match providers::prepare_provider_execution(
            m,
            expected_provider_fingerprint.as_deref(),
        ) {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("warning: model '{m}' cannot be executed: {e}");
                continue;
            }
        };
        let cache_affinity = crate::prompt_context::cache_affinity(
            &context,
            &cache_provider_id(m),
            m,
            prepared_provider.fingerprint.as_deref(),
            cache_policy.clone(),
        );
        if let Some(affinity) = cache_affinity.as_ref() {
            crate::prompt_context::record_cache_affinity(&context, affinity, attempt_index + 1);
            last_cache_affinity = Some(affinity.clone());
        }
        let mut opts = options.clone();
        opts.model = Some(m.clone());
        let attempt_session_id =
            session_id_for_model_attempt(auto_new_session, effective_session_id.as_deref());
        opts.session_id = attempt_session_id.clone();
        last_attempt_session_id = attempt_session_id.clone();
        if let Some(lease) = one_shot_lease.as_mut() {
            lease.bind_attempt(attempt_session_id.as_deref(), m, yolo)?;
        }
        match run_single_turn(HeadlessPrompt::Text(prompt.to_string()), false, opts).await {
            Ok(()) => {
                let tool_calls = if let Some(ref sid) = attempt_session_id {
                    let cwd = cwd
                        .clone()
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                    if has_repeated_tool_call(sid, &cwd, 16) {
                        return Err(anyhow::anyhow!(
                            "anti-loop: same tool call repeated 16 times in a row"
                        ));
                    }
                    count_chat_tool_calls(sid, &cwd)
                } else {
                    0
                };
                let mut data = serde_json::json!({"tool_calls": tool_calls, "success": true});
                if let Some(affinity) = cache_affinity.as_ref() {
                    data["cache_affinity_sha256"] =
                        serde_json::json!(affinity.cache_affinity_sha256);
                }
                if !errors.is_empty() {
                    data["errors"] = serde_json::json!(errors);
                }
                let _ = timeline::add_event(
                    "exec",
                    prompt
                        .split_whitespace()
                        .take(8)
                        .collect::<Vec<_>>()
                        .join(" "),
                    Some(data),
                );
                maybe_auto_create_skill(tool_calls).await;
                record_taste_from_success(prompt).await;
                if let Some(lease) = one_shot_lease.take() {
                    let _ = lease.consume();
                }
                return Ok(());
            }
            Err(e) => {
                eprintln!("warning: model '{m}' failed: {e}");
                errors.push(e.to_string());
                let session_evidence_exists = attempt_session_id.as_deref().is_some_and(|sid| {
                    xai_grok_shell::session::persistence::find_session_dir_by_id(sid).is_some()
                });
                last_result = Err(e);
                if !may_retry_model_attempt(auto_new_session, session_evidence_exists) {
                    break;
                }
            }
        }
    }

    {
        let sid = last_attempt_session_id.as_deref();
        let cwd = cwd
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let tool_calls = sid
            .map(|session_id| count_chat_tool_calls(session_id, &cwd))
            .unwrap_or(0);
        let mut data = serde_json::json!({"tool_calls": tool_calls, "success": false});
        if let Some(affinity) = last_cache_affinity.as_ref() {
            data["cache_affinity_sha256"] = serde_json::json!(affinity.cache_affinity_sha256);
        }
        if !errors.is_empty() {
            data["errors"] = serde_json::json!(errors);
        }
        let _ = timeline::add_event(
            "exec",
            prompt
                .split_whitespace()
                .take(8)
                .collect::<Vec<_>>()
                .join(" "),
            Some(data),
        );
        record_taste_from_failure(prompt, &errors);
    }

    last_result
}

/// Run a single headless turn and return the assistant's text response.
///
/// This is a convenience wrapper for callers that only need the model's final
/// output (e.g. JSON planning or group-chat routing). It creates a throwaway
/// session, caps the turn at one model response, and reads the assistant message
/// back from the session's chat history.
const MAX_CAPTURE_CHAT_HISTORY_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) async fn run_single_turn_capture(
    prompt: &str,
    model: Option<String>,
    yolo: bool,
    max_turns: Option<u32>,
    tools: Option<String>,
) -> Result<String> {
    run_single_turn_capture_with_limit(
        prompt,
        model,
        yolo,
        max_turns,
        tools,
        MAX_CAPTURE_CHAT_HISTORY_BYTES,
    )
    .await
}

pub(crate) async fn run_single_turn_capture_with_limit(
    prompt: &str,
    model: Option<String>,
    yolo: bool,
    max_turns: Option<u32>,
    tools: Option<String>,
    max_chat_history_bytes: u64,
) -> Result<String> {
    run_single_turn_capture_with_limit_and_provider_fingerprint(
        prompt,
        model,
        yolo,
        max_turns,
        tools,
        max_chat_history_bytes,
        None,
    )
    .await
}

pub(crate) async fn run_single_turn_capture_with_provider_fingerprint(
    prompt: &str,
    model: Option<String>,
    yolo: bool,
    max_turns: Option<u32>,
    tools: Option<String>,
    expected_provider_fingerprint: Option<String>,
) -> Result<String> {
    run_single_turn_capture_with_limit_and_provider_fingerprint(
        prompt,
        model,
        yolo,
        max_turns,
        tools,
        MAX_CAPTURE_CHAT_HISTORY_BYTES,
        expected_provider_fingerprint,
    )
    .await
}

async fn run_single_turn_capture_with_limit_and_provider_fingerprint(
    prompt: &str,
    model: Option<String>,
    yolo: bool,
    max_turns: Option<u32>,
    tools: Option<String>,
    max_chat_history_bytes: u64,
    expected_provider_fingerprint: Option<String>,
) -> Result<String> {
    if max_chat_history_bytes == 0 || max_chat_history_bytes > MAX_CAPTURE_CHAT_HISTORY_BYTES {
        bail!("capture history limit must be between 1 and {MAX_CAPTURE_CHAT_HISTORY_BYTES} bytes");
    }
    let candidates = resolve_model_candidates(prompt, model).await?;
    // run_single_turn_capture is used for one-shot text/json capture. If the
    // caller did not supply a tool allowlist, disallow every registered tool so
    // the model returns final text instead of making a tool call in the single
    // capped turn.
    let (cli_tools, cli_disallowed_tools) = match tools.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => (tools.clone(), None),
        _ => (None, Some(all_tool_ids_csv().clone())),
    };
    let mut last_error = anyhow::anyhow!("no usable model candidates for capture");
    for candidate in candidates {
        let session_id = uuid::Uuid::new_v4().to_string();
        let session = SessionParams {
            session_id: Some(session_id.clone()),
            ..Default::default()
        };
        match run_single_turn_with_provider_fingerprint(
            prompt,
            Some(candidate),
            yolo,
            OutputFormat::Plain,
            max_turns,
            cli_tools.clone(),
            cli_disallowed_tools.clone(),
            None,
            None,
            &session,
            false,
            expected_provider_fingerprint.clone(),
        )
        .await
        {
            Ok(()) => {
                return last_assistant_text_for_session(&session_id, max_chat_history_bytes)
                    .await
                    .with_context(|| "failed to capture assistant text");
            }
            Err(run_error) => {
                if xai_grok_shell::session::persistence::find_session_dir_by_id(&session_id)
                    .is_some()
                {
                    return Err(run_error.context(format!(
                        "capture attempt may have produced side effects; session {session_id} was preserved and model fallback was stopped"
                    )));
                }
                last_error = run_error;
            }
        }
    }
    Err(last_error.context("all capture model candidates failed"))
}

async fn last_assistant_text_for_session(
    session_id: &str,
    max_chat_history_bytes: u64,
) -> Result<String> {
    let session_dir = xai_grok_shell::session::persistence::find_session_dir_by_id(session_id)
        .ok_or_else(|| anyhow::anyhow!("session '{session_id}' not found"))?;
    let metadata = tokio::fs::symlink_metadata(&session_dir).await?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "throwaway session path is not a regular directory: {}",
            session_dir.display()
        );
    }
    let text = read_last_assistant_text(&session_dir, session_id, max_chat_history_bytes)
        .await
        .with_context(|| {
            format!(
                "capture failed; session {session_id} was preserved for inspection and recovery"
            )
        })?;
    tokio::fs::remove_dir_all(&session_dir)
        .await
        .with_context(|| {
            format!(
                "remove throwaway session directory {}",
                session_dir.display()
            )
        })?;
    Ok(text)
}

async fn read_last_assistant_text(
    session_dir: &Path,
    session_id: &str,
    max_chat_history_bytes: u64,
) -> Result<String> {
    let path = session_dir.join("chat_history.jsonl");
    let metadata = tokio::fs::symlink_metadata(&path)
        .await
        .with_context(|| format!("inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("session '{session_id}' has no chat history");
    }
    if metadata.len() > max_chat_history_bytes {
        bail!("capture chat history exceeds the {max_chat_history_bytes} byte safety limit");
    }
    let raw = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    parse_last_assistant_text(&raw, &path, session_id)
}

fn parse_last_assistant_text(raw: &str, path: &Path, session_id: &str) -> Result<String> {
    let mut text = String::new();
    for (index, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let item = serde_json::from_str::<xai_grok_shell::sampling::ConversationItem>(line)
            .with_context(|| {
                format!(
                    "invalid JSON record in capture history {} at line {}",
                    path.display(),
                    index + 1
                )
            })?;
        if let xai_grok_shell::sampling::ConversationItem::Assistant(a) = item {
            text = a.content.as_ref().to_string();
        }
    }
    if text.is_empty() {
        bail!("no assistant response in session '{session_id}'");
    }
    Ok(text)
}

const TURN_SENTINEL_NAME: &str = "__omgb_turn__";

static ALL_TOOL_IDS: OnceLock<String> = OnceLock::new();

pub(crate) fn all_tool_ids_csv() -> &'static String {
    ALL_TOOL_IDS.get_or_init(|| {
        let ids: Vec<String> = xai_grok_tools::bridge::ToolBridge::get_builder()
            .known_tool_ids()
            .into_iter()
            .collect();
        ids.join(",")
    })
}

/// Extract the first top-level JSON object from `text`, respecting quoted
/// strings so braces inside string values are not counted. Returns `None` if
/// no valid object is found.
pub(crate) fn extract_json_object(text: &str) -> Option<serde_json::Value> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let start = i;
            let mut depth = 1;
            i += 1;
            let mut in_string = false;
            let mut escape = false;
            while i < bytes.len() && depth > 0 {
                let c = bytes[i];
                if in_string {
                    if escape {
                        escape = false;
                    } else if c == b'\\' {
                        escape = true;
                    } else if c == b'"' {
                        in_string = false;
                    }
                } else if c == b'"' {
                    in_string = true;
                } else if c == b'{' {
                    depth += 1;
                } else if c == b'}' {
                    depth -= 1;
                }
                i += 1;
                if depth == 0 {
                    return serde_json::from_str(&text[start..i]).ok();
                }
            }
            return None;
        }
        i += 1;
    }
    None
}

fn is_turn_sentinel(call: &xai_grok_shell::sampling::ToolCall) -> bool {
    call.name == TURN_SENTINEL_NAME
}

fn count_chat_tool_calls(session_id: &str, cwd: &std::path::Path) -> usize {
    load_chat_tool_calls(session_id, cwd)
        .map(|v| v.iter().filter(|c| !is_turn_sentinel(c)).count())
        .unwrap_or(0)
}

async fn record_taste_from_success(prompt: &str) {
    if std::env::var("OMGB_AUTO_TASTE").ok().as_deref() == Some("0") {
        return;
    }
    if let Ok(diff) = git_diff_text().await
        && !diff.trim().is_empty()
    {
        let diff = crate::prompt_guard::limit_storage(&diff, 4096);
        let _ = crate::taste::taste_accept(prompt, &diff, vec!["auto".into()]);
    }
}

fn record_taste_from_failure(prompt: &str, errors: &[String]) {
    if std::env::var("OMGB_AUTO_TASTE").ok().as_deref() == Some("0") {
        return;
    }
    let output = errors
        .iter()
        .filter(|e| !e.is_empty())
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if !output.is_empty() {
        let output = crate::prompt_guard::limit_storage(&output, 1024);
        let _ = crate::taste::taste_reject(prompt, &output, vec!["auto".into()]);
    }
}

fn load_chat_tool_calls(
    session_id: &str,
    cwd: &std::path::Path,
) -> Option<Vec<xai_grok_shell::sampling::ToolCall>> {
    use xai_grok_shell::sampling::{AssistantItem, ConversationItem, ToolCall};

    let sessions_cwd = xai_grok_shell::util::grok_home::sessions_cwd_dir(&cwd.to_string_lossy());
    let path = sessions_cwd.join(session_id).join("chat_history.jsonl");
    if !path.is_file() {
        return None;
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return None;
    };
    let mut calls = Vec::new();
    for line in raw.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        if let Ok(ConversationItem::Assistant(AssistantItem { tool_calls, .. })) =
            serde_json::from_str::<ConversationItem>(line)
        {
            calls.extend(tool_calls);
            // Sentinel so a following assistant turn breaks a tool-call streak.
            let id = uuid::Uuid::new_v4().to_string();
            calls.push(ToolCall {
                id: id.clone().into(),
                name: TURN_SENTINEL_NAME.into(),
                arguments: id.into(),
            });
        }
    }
    Some(calls)
}

fn has_repeated_tool_call(session_id: &str, cwd: &std::path::Path, threshold: usize) -> bool {
    if threshold == 0 {
        return false;
    }
    let Some(calls) = load_chat_tool_calls(session_id, cwd) else {
        return false;
    };
    let mut last: Option<(String, String)> = None;
    let mut streak = 0;
    for call in calls {
        let key = (call.name.clone(), call.arguments.to_string());
        if last.as_ref() == Some(&key) {
            streak += 1;
        } else {
            last = Some(key);
            streak = 1;
        }
        if streak >= threshold {
            return true;
        }
    }
    false
}

async fn maybe_auto_create_skill(tool_calls: usize) {
    let threshold = std::env::var("OMGB_AUTO_SKILL")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(5);
    if threshold == 0 || tool_calls < threshold {
        return;
    }
    match crate::skill::propose_skill_from_timeline(threshold).await {
        Ok(Some(proposal)) => println!(
            "proposed harness refinement {} for skill '{}' (review with `omgb skill proposal {}`)",
            proposal.id, proposal.candidate.name, proposal.id
        ),
        Ok(None) => {}
        Err(e) => eprintln!("warning: auto skill proposal failed: {e}"),
    }
}

async fn git_worktree_status_in(dir: &std::path::Path) -> Result<(bool, String)> {
    let out = git_cmd()
        .current_dir(dir)
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

async fn git_worktree_status() -> Result<(bool, String)> {
    git_worktree_status_in(&std::env::current_dir()?).await
}

async fn git_diff_text() -> Result<String> {
    let out = git_cmd()
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

async fn git_config_in(dir: &std::path::Path, key: &str) -> Result<Option<String>> {
    let out = git_cmd()
        .current_dir(dir)
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

async fn git_author_in(dir: &std::path::Path) -> Result<(String, String)> {
    if let (Ok(name), Ok(email)) = (
        std::env::var("OMGB_GIT_AUTHOR_NAME"),
        std::env::var("OMGB_GIT_AUTHOR_EMAIL"),
    ) && !name.is_empty()
        && !email.is_empty()
    {
        return Ok((name, email));
    }
    let name = git_config_in(dir, "user.name").await?.ok_or_else(|| {
        anyhow::anyhow!("git author name not configured; set user.name or OMGB_GIT_AUTHOR_NAME")
    })?;
    let email = git_config_in(dir, "user.email").await?.ok_or_else(|| {
        anyhow::anyhow!("git author email not configured; set user.email or OMGB_GIT_AUTHOR_EMAIL")
    })?;
    Ok((name, email))
}

async fn git_author() -> Result<(String, String)> {
    git_author_in(&std::env::current_dir()?).await
}

pub(crate) async fn git_commit_all(
    message: &str,
    include_untracked: bool,
    extra_path: Option<&std::path::Path>,
) -> Result<()> {
    if let Some(path) = extra_path {
        let add = git_cmd().args(["add", "--"]).arg(path).status().await?;
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
    let add = git_cmd().args(["add", add_flag]).status().await?;
    if !add.success() {
        bail!("git add failed");
    }

    let (name, email) = git_author().await?;
    let commit = git_cmd()
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
    let out = git_cmd()
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
    let out = git_cmd()
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

const FEEDBACK_REPO: &str = "josepha-mayo/oh-my-grok-build";

fn is_valid_repo_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn feedback_repo_with(raw: Option<&str>) -> Result<String> {
    if let Some(v) = raw.filter(|s| !s.is_empty()) {
        let parts: Vec<&str> = v.split('/').collect();
        if parts.len() == 2 && is_valid_repo_segment(parts[0]) && is_valid_repo_segment(parts[1]) {
            return Ok(v.to_string());
        }
        bail!(
            "OMGB_FEEDBACK_REPO must be in the form 'owner/repo' with only ASCII alphanumeric, '_', '-', and '.' characters"
        );
    }
    Ok(FEEDBACK_REPO.to_string())
}

fn feedback_repo() -> Result<String> {
    feedback_repo_with(std::env::var("OMGB_FEEDBACK_REPO").ok().as_deref())
}

async fn run_feedback(args: FeedbackArgs) -> Result<()> {
    let body = args
        .message
        .as_deref()
        .unwrap_or("<describe your issue or suggestion here>");
    let title = "Feedback from omgb user";
    let repo = feedback_repo()?;
    let encoded_title = urlencoding::encode(title);
    let encoded_body = urlencoding::encode(body);
    let url =
        format!("https://github.com/{repo}/issues/new?title={encoded_title}&body={encoded_body}");
    if args.open {
        let mut cmd;
        if cfg!(target_os = "windows") {
            cmd = std::process::Command::new("cmd");
            cmd.args(["/c", "start", ""]);
        } else if cfg!(target_os = "macos") {
            cmd = std::process::Command::new("open");
        } else {
            cmd = std::process::Command::new("xdg-open");
        }
        cmd.arg(&url);
        match cmd.status() {
            Ok(s) if s.success() => {
                println!("opened feedback page in browser");
                return Ok(());
            }
            _ => {}
        }
    }
    println!("Submit feedback at:\n{url}");
    Ok(())
}

fn load_brief() -> Option<String> {
    const MAX_BRIEF_BYTES: u64 = 64 * 1024;
    let mut dirs = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home);
    }
    for mut dir in dirs {
        // Resolve the starting directory so we do not search through symlinked
        // parents into unintended locations.
        dir = dunce::canonicalize(&dir).unwrap_or(dir);
        loop {
            let path = dir.join("oh_my_grok_build_brief.md");
            // Use symlink_metadata so a symlink cannot point to an arbitrary file.
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                if meta.is_symlink() {
                    return None;
                }
                if meta.is_file() {
                    if meta.len() > MAX_BRIEF_BYTES {
                        eprintln!(
                            "warning: {} is larger than {} bytes; skipping brief",
                            path.display(),
                            MAX_BRIEF_BYTES
                        );
                        return None;
                    }
                    return read_prompt_file(&path).ok();
                }
            }
            if !dir.pop() {
                break;
            }
        }
    }
    None
}

async fn run_loop(args: LoopArgs) -> Result<()> {
    const MAX_DIFF_CHARS: usize = 16 * 1024;
    const TOOL_CALL_LOOP_THRESHOLD: usize = 16;

    if !args.yolo {
        bail!("`omgb loop` requires --yolo to auto-approve tool use");
    }

    if !git_worktree_status().await?.0 {
        bail!("git working tree is not clean; commit or stash changes before running `omgb loop`");
    }

    let mut session = args.session.clone();
    if session.session_id.is_none() && !session.continue_last {
        session.session_id = Some(uuid::Uuid::new_v4().to_string());
    }
    let session_id = session.session_id.clone().unwrap_or_default();
    let cwd = std::env::current_dir()?;
    let brief = load_brief();

    let mut iteration = 0;
    let base_prompt = args.prompt.clone();
    let mut prompt = brief
        .as_ref()
        .map_or_else(|| base_prompt.clone(), |b| format!("{b}\n\n{base_prompt}"));
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
            args.max_turns,
            None,
            None,
            None,
            None,
            &session,
            args.memory,
        )
        .await?;

        if has_repeated_tool_call(&session_id, &cwd, TOOL_CALL_LOOP_THRESHOLD) {
            bail!("anti-loop: same tool call repeated {TOOL_CALL_LOOP_THRESHOLD} times in a row");
        }

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
        let next = format!(
            "Original task: {}\n\nCurrent git changes:\n{}\n\nContinue until complete.",
            args.prompt, changes
        );
        prompt = brief
            .as_ref()
            .map_or_else(|| next.clone(), |b| format!("{b}\n\n{next}"));
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
            let default_model = configured_default_model()?;
            for p in list_providers()? {
                let default = if Some(format!("omgb-{}", p.id)) == default_model {
                    " (default)"
                } else {
                    ""
                };
                let cost =
                    moe::provider_cost_with_override(&p.id, Some(&p.base_url), p.cost_per_million);
                let cost_source = if p.cost_per_million.is_some() {
                    "configured"
                } else {
                    "fallback estimate"
                };
                println!(
                    "{}{} - {} -> {} (routing cost: ${cost:.4}/1M, {cost_source})",
                    p.id, default, p.name, p.base_url
                );
            }
        }
        ProviderCommand::Catalog => {
            for t in catalog::TEMPLATES {
                let key = t
                    .env_key
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| crate::providers::env_var_name(t.id));
                println!("{} - {} -> {} (key: {})", t.id, t.name, t.base_url, key);
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
        ProviderCommand::Cost { id, value, reset } => {
            if reset {
                let p = set_provider_cost(&id, None)?;
                let fallback = moe::provider_cost(&p.id, Some(&p.base_url));
                println!(
                    "provider {} routing cost reset to fallback estimate ${fallback:.4}/1M tokens",
                    p.id
                );
            } else if let Some(value) = value {
                let p = set_provider_cost(&id, Some(value))?;
                println!(
                    "provider {} routing cost set to ${value:.4}/1M tokens",
                    p.id
                );
            } else {
                let p = get_provider(&id)?
                    .ok_or_else(|| anyhow::anyhow!("provider '{id}' not found"))?;
                let value =
                    moe::provider_cost_with_override(&p.id, Some(&p.base_url), p.cost_per_million);
                let source = if p.cost_per_million.is_some() {
                    "configured override"
                } else {
                    "fallback estimate"
                };
                println!(
                    "provider {} routing cost: ${value:.4}/1M tokens ({source})",
                    p.id
                );
            }
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
            let report = test_provider(&id).await?;
            println!(
                "provider {}: ok (model={}, backend={}, endpoint={}, model_list={}, inference_probe={})",
                report.provider_id,
                report.model,
                report.backend,
                report.endpoint,
                if report.model_list_supported && report.model_advertised {
                    "verified"
                } else {
                    "unavailable"
                },
                if report.inference_verified {
                    "verified"
                } else {
                    "not-needed"
                }
            );
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
            let selected = if let Some(id) = configured_provider_switch_id(&model)? {
                providers::set_default_provider(&id)?;
                format!("omgb-{id}")
            } else {
                providers::set_grok_default_model(&model)?;
                model
            };
            println!("default model switched to {selected}");
        }
    }
    Ok(())
}

fn configured_provider_switch_id(model: &str) -> Result<Option<String>> {
    if let Some(id) = model.strip_prefix("omgb-") {
        return Ok(Some(id.to_string()));
    }
    Ok(providers::get_provider(model)?.map(|provider| provider.id))
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
        ScheduleCommand::ResolveRun { name, confirm } => resolve_job_run(&name, confirm).await,
        ScheduleCommand::SetExpiry { name, expires_at } => {
            omgb_schedule_set_expiry(&name, expires_at.as_deref()).await?;
            println!("set expiry for '{name}'");
            Ok(())
        }
        ScheduleCommand::CleanupExpired => {
            let removed = omgb_schedule_cleanup_expired().await?;
            println!("removed {removed} expired job(s)");
            Ok(())
        }
        ScheduleCommand::Start => spawn_daemon().await,
        ScheduleCommand::Daemon => run_daemon_loop().await,
        ScheduleCommand::Stop => stop_daemon(),
    }
}

const MAX_TEAM_AGENTS: usize = 16;
const MAX_SWARM_MEMBERS: usize = 16;

fn validate_parallel_count(kind: &str, count: usize, max: usize) -> Result<()> {
    if !(1..=max).contains(&count) {
        bail!("{kind} count must be between 1 and {max}");
    }
    Ok(())
}

fn ensure_team_completed(
    total: usize,
    failed_agents: usize,
    failed_merges: usize,
    cleanup_failures: usize,
) -> Result<()> {
    if failed_agents > 0 || failed_merges > 0 || cleanup_failures > 0 {
        bail!(
            "team run incomplete: {failed_agents} of {total} agent(s) failed, {failed_merges} merge(s) failed, and {cleanup_failures} cleanup operation(s) failed"
        );
    }
    Ok(())
}

async fn git_current_branch(repo_root: &std::path::Path) -> Result<String> {
    let out = git_cmd()
        .current_dir(repo_root)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .await?;
    if !out.status.success() {
        bail!("team mode requires a checked-out branch (detached HEAD is not supported)");
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if branch.is_empty() {
        bail!("team mode could not determine the current branch");
    }
    Ok(branch)
}

async fn run_team(args: TeamArgs) -> Result<()> {
    if !args.yolo {
        bail!("`omgb team` requires --yolo to auto-approve tool use");
    }
    validate_parallel_count("team agent", args.agents, MAX_TEAM_AGENTS)?;
    let repo_root = git_repo_root().await?;
    let git = git_cmd()
        .current_dir(&repo_root)
        .args(["rev-parse", "--git-dir"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    if !git.success() {
        bail!("team mode requires a git repository");
    }
    if !git_worktree_status_in(&repo_root).await?.0 {
        bail!("git working tree is not clean; commit or stash changes before running `omgb team`");
    }
    let base_branch = git_current_branch(&repo_root).await?;

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
        let repo_root = repo_root.clone();
        let base_branch = base_branch.clone();

        tasks.push(async move {
            let branch = create_worktree(&repo_root, &worktree, &base_branch).await?;
            let run_result = run_single_turn_with(
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
            .await;
            Ok::<_, anyhow::Error>((worktree, branch, run_result))
        });
    }

    let mut worktrees = Vec::new();
    let mut failed_agent_worktrees = Vec::new();
    let mut failed_agents = 0_usize;
    let mut cleanup_failures = Vec::new();
    for result in futures::future::join_all(tasks).await {
        match result {
            Ok((worktree, branch, Ok(()))) => worktrees.push((worktree, branch)),
            Ok((worktree, branch, Err(e))) => {
                eprintln!("agent failed: {e}");
                eprintln!(
                    "warning: preserving failed agent worktree {} on branch {branch} for recovery",
                    worktree.display()
                );
                failed_agents += 1;
                failed_agent_worktrees.push((worktree, branch));
            }
            Err(e) => {
                failed_agents += 1;
                eprintln!("agent failed before its worktree could be retained: {e}");
            }
        }
    }
    if worktrees.is_empty() {
        for (path, branch) in &failed_agent_worktrees {
            eprintln!(
                "warning: retained failed agent worktree {} on branch {branch}",
                path.display()
            );
        }
        bail!("all team agents failed");
    }

    let mut failed_merges = Vec::new();
    for (w, branch) in &worktrees {
        if let Err(e) = merge_worktree_into_base(&repo_root, w, branch, &base_branch).await {
            eprintln!(
                "warning: failed to merge worktree {}: {e}; leaving it for manual resolution",
                w.display()
            );
            failed_merges.push((w.clone(), branch.clone()));
            continue;
        }
        if let Err(e) = cleanup_team_worktree(&repo_root, w, branch).await {
            eprintln!(
                "warning: changes were merged but cleanup failed for {} ({branch}): {e}",
                w.display()
            );
            cleanup_failures.push((w.clone(), branch.clone()));
        }
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
    for (path, branch) in &failed_merges {
        eprintln!(
            "warning: retained worktree {} on branch {branch}",
            path.display()
        );
    }
    for (path, branch) in &failed_agent_worktrees {
        eprintln!(
            "warning: failed agent work is retained at {} on branch {branch}",
            path.display()
        );
    }
    if !cleanup_failures.is_empty() {
        for (path, branch) in &cleanup_failures {
            eprintln!(
                "warning: cleanup still required for {} on branch {branch}",
                path.display()
            );
        }
    }
    ensure_team_completed(
        args.agents,
        failed_agents,
        failed_merges.len(),
        cleanup_failures.len(),
    )
}

async fn create_worktree(
    repo_root: &std::path::Path,
    path: &PathBuf,
    base_branch: &str,
) -> Result<String> {
    let branch = format!("omgb-team-{}", uuid::Uuid::new_v4());
    let out = git_cmd()
        .current_dir(repo_root)
        .args(["worktree", "add", "-b", &branch, "-q"])
        .arg(path)
        .arg(base_branch)
        .output()
        .await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git worktree add failed: {stderr}");
    }
    Ok(branch)
}

async fn remove_worktree(repo_root: &std::path::Path, path: &PathBuf) -> Result<()> {
    let out = git_cmd()
        .current_dir(repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .output()
        .await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "git worktree remove failed for {}: {stderr}",
            path.display()
        );
    }
    if path.exists() {
        bail!(
            "git reported successful worktree removal but {} still exists",
            path.display()
        );
    }
    Ok(())
}

async fn cleanup_team_worktree(
    repo_root: &std::path::Path,
    path: &PathBuf,
    branch: &str,
) -> Result<()> {
    if !branch.starts_with("omgb-team-") {
        bail!("refusing to delete non-team branch '{branch}'");
    }
    remove_worktree(repo_root, path).await?;
    let delete = git_cmd()
        .current_dir(repo_root)
        .args(["branch", "-D", "--", branch])
        .output()
        .await?;
    if !delete.status.success() {
        let stderr = String::from_utf8_lossy(&delete.stderr);
        bail!("failed to delete team branch {branch}: {stderr}");
    }
    Ok(())
}

async fn merge_worktree_into_base(
    repo_root: &std::path::Path,
    worktree: &PathBuf,
    branch: &str,
    base_branch: &str,
) -> Result<()> {
    let stage = git_cmd()
        .current_dir(worktree)
        .args(["add", "-A"])
        .status()
        .await?;
    if !stage.success() {
        bail!("git add -A in worktree failed");
    }

    let diff = git_cmd()
        .current_dir(worktree)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .await?;
    let mut git_identity = None;
    if !diff.success() {
        let (name, email) = git_author_in(repo_root).await?;
        let commit_msg = format!("omgb team agent {branch}");
        let commit = git_cmd()
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
        git_identity = Some((name, email));
    } else {
        let already_merged = git_cmd()
            .current_dir(repo_root)
            .args(["merge-base", "--is-ancestor", branch, base_branch])
            .status()
            .await?;
        if already_merged.success() {
            return Ok(());
        }
        if already_merged.code() != Some(1) {
            bail!("failed to compare team branch '{branch}' with base branch '{base_branch}'");
        }
    }

    let (clean, _) = git_worktree_status_in(repo_root).await?;
    if !clean {
        bail!("base working tree is not clean; commit or stash changes before merging team output");
    }
    let current_branch = git_current_branch(repo_root).await?;
    if current_branch != base_branch {
        bail!(
            "base checkout moved from branch '{base_branch}' to '{current_branch}' while team agents were running"
        );
    }

    let merge_msg = format!("Merge omgb team agent {branch}");
    let (name, email) = match git_identity {
        Some(identity) => identity,
        None => git_author_in(repo_root).await?,
    };
    let merge = git_cmd()
        .current_dir(repo_root)
        .env("GIT_AUTHOR_NAME", &name)
        .env("GIT_AUTHOR_EMAIL", &email)
        .env("GIT_COMMITTER_NAME", &name)
        .env("GIT_COMMITTER_EMAIL", &email)
        .args([
            "merge",
            "--no-ff",
            "-m",
            merge_msg.as_str(),
            "--no-gpg-sign",
            branch,
        ])
        .output()
        .await?;
    if !merge.status.success() {
        let stderr = String::from_utf8_lossy(&merge.stderr).trim().to_string();
        let abort = git_cmd()
            .current_dir(repo_root)
            .args(["merge", "--abort"])
            .output()
            .await?;
        if !abort.status.success() {
            let abort_stderr = String::from_utf8_lossy(&abort.stderr).trim().to_string();
            bail!(
                "git merge failed ({stderr}) and merge abort failed ({abort_stderr}); inspect {} before continuing",
                repo_root.display()
            );
        }
        bail!("git merge failed and was aborted: {stderr}");
    }
    Ok(())
}

async fn run_swarm(args: SwarmArgs) -> Result<()> {
    if !args.yolo {
        bail!("`omgb swarm` requires --yolo to auto-approve tool use");
    }
    validate_parallel_count("swarm member", args.count, MAX_SWARM_MEMBERS)?;
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
        SubagentCommand::Kill { id } => subagents::kill(&id).await,
        SubagentCommand::Logs { id } => subagents::logs(&id).await,
        SubagentCommand::Trace { id } => subagents::trace(&id).await,
        SubagentCommand::Worker {
            prompt_file,
            stdout_path,
            stderr_path,
            admission_path,
            admission_token,
            yolo,
        } => {
            subagents::run_worker(
                &prompt_file,
                &stdout_path,
                &stderr_path,
                &admission_path,
                &admission_token,
                yolo,
            )
            .await
        }
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
        TasteCommand::Accept {
            prompt,
            output,
            tags,
        } => {
            taste::taste_accept(&prompt, &output, tags)?;
            println!("recorded accept");
        }
        TasteCommand::Reject {
            prompt,
            output,
            tags,
        } => {
            taste::taste_reject(&prompt, &output, tags)?;
            println!("recorded reject");
        }
        TasteCommand::Edit {
            prompt,
            before,
            after,
            tags,
        } => {
            taste::taste_edit(&prompt, &before, &after, tags)?;
            println!("recorded edit");
        }
        TasteCommand::List => taste::list_taste()?,
    }
    Ok(())
}

async fn run_skill(args: SkillArgs) -> Result<()> {
    match args.command {
        SkillCommand::List => {
            for skill in crate::skill::list_skills()? {
                println!("{} (trigger: {})", skill.name, skill.trigger);
            }
        }
        SkillCommand::Show { name } => {
            for skill in crate::skill::list_skills()? {
                if skill.name.eq_ignore_ascii_case(&name) {
                    let body = crate::skill::format_skill_markdown(&skill)?;
                    println!("{body}");
                    return Ok(());
                }
            }
            eprintln!("skill not found: {name}");
        }
        SkillCommand::AutoCreate { threshold } => {
            if let Some(proposal) = crate::skill::propose_skill_from_timeline(threshold).await? {
                println!(
                    "proposed refinement {} for skill '{}' (not active; review then run `omgb skill approve {} --confirm`)",
                    proposal.id, proposal.candidate.name, proposal.id
                );
            } else {
                println!("no suitable timeline run found");
            }
        }
        SkillCommand::Proposals => {
            let proposals = crate::skill::list_proposals()?;
            if proposals.is_empty() {
                println!("no refinement proposals");
            }
            for proposal in proposals {
                println!(
                    "{} [{:?}] {} source={} candidate={}",
                    proposal.id,
                    proposal.status,
                    proposal.candidate.name,
                    proposal.source_sha256,
                    proposal.candidate_sha256
                );
            }
        }
        SkillCommand::Proposal { id } => {
            let proposal = crate::skill::load_proposal(&id)?;
            println!(
                "proposal {} [{:?}]\ncreated: {}\nsource sha256: {}\ncandidate sha256: {}\nnote: {}\n\n{}",
                proposal.id,
                proposal.status,
                proposal.created_at.to_rfc3339(),
                proposal.source_sha256,
                proposal.candidate_sha256,
                proposal.note.as_deref().unwrap_or("-"),
                crate::skill::format_skill_markdown(&proposal.candidate)?
            );
        }
        SkillCommand::Approve { id, confirm } => {
            let proposal = crate::skill::approve_proposal(&id, confirm)?;
            println!(
                "activated refinement {} for skill '{}'",
                proposal.id, proposal.candidate.name
            );
        }
        SkillCommand::Reject { id, reason } => {
            let proposal = crate::skill::reject_proposal(&id, reason)?;
            println!("rejected refinement {}", proposal.id);
        }
        SkillCommand::Rollback { id, confirm } => {
            let proposal = crate::skill::rollback_proposal(&id, confirm)?;
            println!(
                "rolled back refinement {} for skill '{}'",
                proposal.id, proposal.candidate.name
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    fn noop_waker() -> Waker {
        struct Noop;
        impl Wake for Noop {
            fn wake(self: Arc<Self>) {}
            fn wake_by_ref(self: &Arc<Self>) {}
        }
        Waker::from(Arc::new(Noop))
    }

    #[test]
    fn bounded_capture_keeps_prefix_and_discards_rest() {
        let mut capture = BoundedCapture::new(5);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let res = {
            let mut pinned = Pin::new(&mut capture);
            pinned.as_mut().poll_write(&mut cx, b"hello world")
        };
        assert!(matches!(res, Poll::Ready(Ok(11))));
        assert_eq!(capture.into_string(), "hello");
    }

    #[test]
    fn model_switch_accepts_bare_configured_provider_id() {
        let _guard = OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home =
            std::env::temp_dir().join(format!("omgb-model-switch-test-{}", uuid::Uuid::new_v4()));
        providers::set_omg_home_for_tests(Some(home.clone()));
        let provider = providers::ProviderConfig {
            id: "codex".into(),
            name: "OpenAI Codex".into(),
            model: "codex-mini-latest".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_backend: Some("responses".into()),
            env_key: Some(vec!["OPENAI_API_KEY".into()]),
            no_auth: false,
            extra_headers: None,
            context_window: None,
            auto_compact_threshold_percent: None,
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
            cost_per_million: None,
        };
        providers::save_omg_config(&providers::OmgConfig {
            providers: std::collections::HashMap::from([("codex".into(), provider)]),
            ..providers::OmgConfig::default()
        })
        .unwrap();

        assert_eq!(
            configured_provider_switch_id("codex").unwrap(),
            Some("codex".into())
        );
        assert_eq!(
            configured_provider_switch_id("omgb-codex").unwrap(),
            Some("codex".into())
        );

        providers::set_omg_home_for_tests(None);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn model_fallback_uses_fresh_auto_session_ids_but_preserves_explicit_ids() {
        let first = session_id_for_model_attempt(true, None).unwrap();
        let second = session_id_for_model_attempt(true, None).unwrap();
        assert_ne!(first, second);

        let explicit = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            session_id_for_model_attempt(false, Some(&explicit)),
            Some(explicit)
        );
        assert!(may_retry_model_attempt(true, false));
        assert!(!may_retry_model_attempt(true, true));
        assert!(!may_retry_model_attempt(false, false));
    }

    #[test]
    fn extract_json_object_respects_quoted_braces() {
        let text = r#"prefix {"replies":{"Alice":"ok {not closed","Bob":"hi"}} suffix"#;
        let value = extract_json_object(text).unwrap();
        let replies = value.get("replies").unwrap().as_object().unwrap();
        assert_eq!(replies["Alice"].as_str().unwrap(), "ok {not closed");
        assert_eq!(replies["Bob"].as_str().unwrap(), "hi");
    }

    #[test]
    fn capture_history_returns_the_last_assistant_and_rejects_corruption() {
        let path = Path::new("chat_history.jsonl");
        let raw = concat!(
            "{\"type\":\"assistant\",\"content\":\"first\"}\n",
            "{\"type\":\"assistant\",\"content\":\"second\"}\n"
        );
        assert_eq!(
            parse_last_assistant_text(raw, path, "session").unwrap(),
            "second"
        );
        let error = parse_last_assistant_text("{SUPERSECRET}\n", path, "session")
            .unwrap_err()
            .to_string();
        assert!(error.contains("line 1"));
        assert!(!error.contains("SUPERSECRET"));
    }

    fn tmp_test_dir() -> PathBuf {
        let tmp = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("tmp-tests-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    fn cleanup_tmp_dir(tmp: &PathBuf) {
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn test_resolve_path_accepts_relative_file() {
        let tmp = tmp_test_dir();
        let rel = std::path::Path::new("target")
            .join(tmp.file_name().unwrap())
            .join("out.txt");
        let resolved = resolve_path(&rel).unwrap();
        assert!(
            resolved.ends_with(&rel),
            "resolved path should end with the relative input"
        );
        cleanup_tmp_dir(&tmp);
    }

    #[test]
    fn test_resolve_path_rejects_dot_and_empty() {
        assert!(resolve_path(std::path::Path::new(".")).is_err());
        assert!(resolve_path(std::path::Path::new("./")).is_err());
    }

    #[test]
    fn test_resolve_path_rejects_absolute_and_parent_dir() {
        assert!(resolve_path(std::path::Path::new("/etc/passwd")).is_err());
        assert!(resolve_path(std::path::Path::new("foo/../bar")).is_err());
        assert!(resolve_path(std::path::Path::new("../outside")).is_err());
    }

    #[test]
    fn test_resolve_path_rejects_directory() {
        let tmp = tmp_test_dir();
        let rel = std::path::Path::new("target").join(tmp.file_name().unwrap());
        assert!(
            resolve_path(&rel).is_err(),
            "existing directory should be rejected"
        );
        cleanup_tmp_dir(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_path_rejects_symlink() {
        use std::os::unix::fs::symlink;
        let tmp = tmp_test_dir();
        let real = tmp.join("real.txt");
        std::fs::write(&real, "x").unwrap();
        let link = tmp.join("link");
        symlink(&real, &link).unwrap();
        let rel = std::path::Path::new("target")
            .join(tmp.file_name().unwrap())
            .join("link");
        assert!(resolve_path(&rel).is_err(), "symlink should be rejected");
        cleanup_tmp_dir(&tmp);
    }

    #[test]
    fn test_feedback_repo_validation() {
        assert_eq!(feedback_repo_with(None).unwrap(), FEEDBACK_REPO);
        assert_eq!(
            feedback_repo_with(Some("myorg/myrepo")).unwrap(),
            "myorg/myrepo"
        );
        assert!(feedback_repo_with(Some("foo")).is_err());
        assert!(feedback_repo_with(Some("foo/../bar")).is_err());
        assert!(feedback_repo_with(Some("foo/bar?x=1")).is_err());
        assert!(feedback_repo_with(Some("foo//bar")).is_err());
    }

    #[test]
    fn test_process_alive_detects_current_process() {
        assert!(
            crate::process_alive(std::process::id()),
            "current process should be alive"
        );
    }

    #[test]
    fn test_process_alive_nonexistent_pid() {
        assert!(
            !crate::process_alive(u32::MAX),
            "non-existent PID should not be alive"
        );
    }

    #[test]
    fn parallel_agent_counts_are_bounded() {
        assert!(validate_parallel_count("team agent", 1, MAX_TEAM_AGENTS).is_ok());
        assert!(validate_parallel_count("team agent", MAX_TEAM_AGENTS, MAX_TEAM_AGENTS).is_ok());
        assert!(validate_parallel_count("team agent", 0, MAX_TEAM_AGENTS).is_err());
        assert!(
            validate_parallel_count("team agent", MAX_TEAM_AGENTS + 1, MAX_TEAM_AGENTS).is_err()
        );
        assert!(validate_parallel_count("swarm member", 0, MAX_SWARM_MEMBERS).is_err());
        assert!(
            validate_parallel_count("swarm member", MAX_SWARM_MEMBERS + 1, MAX_SWARM_MEMBERS)
                .is_err()
        );
    }

    #[test]
    fn partial_team_merge_is_not_reported_as_success() {
        assert!(ensure_team_completed(2, 1, 0, 0).is_err());
        assert!(ensure_team_completed(2, 0, 1, 0).is_err());
        assert!(ensure_team_completed(2, 0, 0, 1).is_err());
        assert!(ensure_team_completed(2, 0, 0, 0).is_ok());
    }

    async fn init_team_test_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let init = git_cmd()
            .current_dir(repo)
            .args(["init", "-q", "-b", "trunk"])
            .status()
            .await
            .unwrap();
        assert!(init.success());
        for (key, value) in [
            ("user.name", "OMGB Team Test"),
            ("user.email", "team-test@example.invalid"),
        ] {
            let status = git_cmd()
                .current_dir(repo)
                .args(["config", key, value])
                .status()
                .await
                .unwrap();
            assert!(status.success());
        }
        std::fs::write(repo.join("shared.txt"), "base\n").unwrap();
        let add = git_cmd()
            .current_dir(repo)
            .args(["add", "shared.txt"])
            .status()
            .await
            .unwrap();
        assert!(add.success());
        let commit = git_cmd()
            .current_dir(repo)
            .args(["commit", "-q", "--no-gpg-sign", "-m", "base"])
            .status()
            .await
            .unwrap();
        assert!(commit.success());
        temp
    }

    #[tokio::test]
    async fn team_cleanup_removes_worktree_before_branch() {
        let temp = init_team_test_repo().await;
        let repo = temp.path();
        let worktree =
            std::env::temp_dir().join(format!("omgb-team-cleanup-test-{}", uuid::Uuid::new_v4()));
        let branch = create_worktree(repo, &worktree, "trunk").await.unwrap();
        assert!(worktree.is_dir());

        cleanup_team_worktree(repo, &worktree, &branch)
            .await
            .unwrap();

        assert!(!worktree.exists());
        let branches = git_cmd()
            .current_dir(repo)
            .args(["branch", "--list", &branch])
            .output()
            .await
            .unwrap();
        assert!(branches.status.success());
        assert!(branches.stdout.is_empty());
    }

    #[tokio::test]
    async fn team_no_change_agent_does_not_leak_branch() {
        let temp = init_team_test_repo().await;
        let repo = temp.path();
        let worktree =
            std::env::temp_dir().join(format!("omgb-team-no-change-test-{}", uuid::Uuid::new_v4()));
        let branch = create_worktree(repo, &worktree, "trunk").await.unwrap();

        merge_worktree_into_base(repo, &worktree, &branch, "trunk")
            .await
            .unwrap();
        cleanup_team_worktree(repo, &worktree, &branch)
            .await
            .unwrap();

        let branches = git_cmd()
            .current_dir(repo)
            .args(["branch", "--list", &branch])
            .output()
            .await
            .unwrap();
        assert!(branches.status.success());
        assert!(branches.stdout.is_empty());
        assert!(!worktree.exists());
    }

    #[tokio::test]
    async fn team_agent_commits_are_merged_even_when_the_index_is_clean() {
        let temp = init_team_test_repo().await;
        let repo = temp.path();
        let worktree =
            std::env::temp_dir().join(format!("omgb-team-commit-test-{}", uuid::Uuid::new_v4()));
        let branch = create_worktree(repo, &worktree, "trunk").await.unwrap();
        std::fs::write(worktree.join("agent.txt"), "committed output\n").unwrap();
        let commit = git_cmd()
            .current_dir(&worktree)
            .args(["add", "agent.txt"])
            .status()
            .await
            .unwrap();
        assert!(commit.success());
        let commit = git_cmd()
            .current_dir(&worktree)
            .args(["commit", "-q", "--no-gpg-sign", "-m", "agent commit"])
            .status()
            .await
            .unwrap();
        assert!(commit.success());

        merge_worktree_into_base(repo, &worktree, &branch, "trunk")
            .await
            .unwrap();
        cleanup_team_worktree(repo, &worktree, &branch)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(repo.join("agent.txt"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ["committed output"]
        );
    }

    #[tokio::test]
    async fn team_merge_conflict_is_aborted_and_base_stays_clean() {
        let temp = init_team_test_repo().await;
        let repo = temp.path();
        let worktree =
            std::env::temp_dir().join(format!("omgb-team-conflict-test-{}", uuid::Uuid::new_v4()));
        let branch = create_worktree(repo, &worktree, "trunk").await.unwrap();

        std::fs::write(repo.join("shared.txt"), "base changed\n").unwrap();
        let base_commit = git_cmd()
            .current_dir(repo)
            .args(["commit", "-qam", "base change", "--no-gpg-sign"])
            .status()
            .await
            .unwrap();
        assert!(base_commit.success());
        std::fs::write(worktree.join("shared.txt"), "agent changed\n").unwrap();

        let error = merge_worktree_into_base(repo, &worktree, &branch, "trunk")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("was aborted"));
        assert!(git_worktree_status_in(repo).await.unwrap().0);
        assert_eq!(git_current_branch(repo).await.unwrap(), "trunk");
        let merge_head = git_cmd()
            .current_dir(repo)
            .args(["rev-parse", "--quiet", "--verify", "MERGE_HEAD"])
            .status()
            .await
            .unwrap();
        assert!(!merge_head.success());

        cleanup_team_worktree(repo, &worktree, &branch)
            .await
            .unwrap();
    }
}
