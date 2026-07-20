use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, ValueEnum)]
pub enum ApiBackend {
    ChatCompletions,
    Responses,
    Messages,
}

impl ApiBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiBackend::ChatCompletions => "chat_completions",
            ApiBackend::Responses => "responses",
            ApiBackend::Messages => "messages",
        }
    }
}

#[derive(Debug, Parser, Clone)]
#[command(name = "oh-my-grok-build", about = "Oh My Grok Build harness", version)]
pub struct OmgbArgs {
    #[command(subcommand)]
    pub command: Option<OmgbCommand>,
}

#[derive(Debug, Subcommand, Clone)]
pub enum OmgbCommand {
    /// Run the TUI (default when no subcommand is given)
    Tui(TuiArgs),
    /// Single-turn headless prompt
    Exec(ExecArgs),
    /// Autonomous diff-driven work loop
    Loop(LoopArgs),
    /// High-autonomy mode (same as exec with --yolo and guard checks)
    Autonomous(AutonomousArgs),
    /// Manage BYOK / local model providers
    Provider(ProviderArgs),
    /// List or switch models
    Model(ModelArgs),
    /// Schedule background prompts (cron-style)
    Cron(CronArgs),
    /// Manage scheduled jobs
    #[command(alias = "sched")]
    Schedule(ScheduleArgs),
    /// Team mode with isolated git worktrees
    Team(TeamArgs),
    /// Parallel subagent swarm
    Swarm(SwarmArgs),
    /// Spawn/list/kill/logs/trace subagents
    Subagent(SubagentArgs),
    /// Deep arXiv / web research and patch proposal
    Research(ResearchArgs),
    /// Show recent session/job events
    Timeline(TimelineArgs),
    /// Drive OpenCode, Codex, Claude, Hermes, Pi, OMP CLI agents
    Harness(HarnessArgs),
    /// Start the ACP relay for the mobile app
    Serve(ServeArgs),
    /// Connect to an ACP relay
    Connect(ConnectArgs),
    /// Computer use prompt
    Use(UseArgs),
    /// Browser use prompt
    Browser(BrowserArgs),
    /// Manage MCP servers
    Mcp(xai_grok_pager::mcp_cmd::McpArgs),
    /// Remember a coding-style preference
    Taste(TasteArgs),
    /// Commit the current working tree
    Commit(CommitArgs),
    /// Review current changes (git status + diff)
    Review,
    /// Undo the last omgb commit
    Undo(UndoArgs),
}

#[derive(Debug, Args, Clone, Default)]
pub struct TuiArgs {
    /// Initial prompt
    pub prompt: Option<String>,
    #[arg(short, long)]
    pub model: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct ExecArgs {
    pub prompt: Option<String>,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long, value_name = "FILE")]
    pub prompt_file: Option<PathBuf>,
    #[arg(long)]
    pub yolo: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub output_file: Option<PathBuf>,
    /// Commit changes after the prompt finishes
    #[arg(long)]
    pub commit: bool,
    /// Stage and commit untracked files as well
    #[arg(long)]
    pub commit_untracked: bool,
}

#[derive(Debug, Args, Clone)]
pub struct LoopArgs {
    pub prompt: String,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(short = 'n', long, default_value = "10")]
    pub max_iterations: u32,
    #[arg(long)]
    pub yolo: bool,
    /// Commit changes after the loop finishes (required for auto-commit)
    #[arg(long)]
    pub commit: bool,
    /// Stage and commit untracked files as well as tracked changes
    #[arg(long)]
    pub commit_untracked: bool,
}

#[derive(Debug, Args, Clone)]
pub struct AutonomousArgs {
    pub prompt: String,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long, default_value = "workspace")]
    pub sandbox_profile: String,
}

#[derive(Debug, Args, Clone)]
pub struct ProviderArgs {
    #[command(subcommand)]
    pub command: ProviderCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum ProviderCommand {
    /// List configured providers
    List,
    /// Add a provider from a built-in template or custom values
    Add(AddProviderArgs),
    /// Remove a provider
    Remove { id: String },
    /// Discover local models (Ollama / LM Studio)
    Discover(DiscoverArgs),
    /// Test a provider's connectivity
    Test { id: String },
}

#[derive(Debug, Args, Clone)]
pub struct AddProviderArgs {
    /// Provider id (e.g. openai, anthropic, ollama)
    pub id: String,
    /// Built-in template to use
    #[arg(short, long)]
    pub template: Option<String>,
    /// Display name
    #[arg(short, long)]
    pub name: Option<String>,
    /// Model name
    #[arg(short, long)]
    pub model: Option<String>,
    /// Base URL
    #[arg(long)]
    pub base_url: Option<String>,
    /// API backend (defaults to chat-completions for custom providers)
    #[arg(long, value_enum)]
    pub backend: Option<ApiBackend>,
    /// API key value to store (also reads OMGB_API_KEY env var if omitted)
    #[arg(long, env = "OMGB_API_KEY")]
    pub api_key: Option<String>,
    /// Environment variable name(s) to read the key from at runtime (defaults to OMGB_<id>_API_KEY, plus the canonical env var for built-in templates)
    #[arg(long)]
    pub env_key: Option<String>,
    /// Context window in tokens for this model
    #[arg(long)]
    pub context_window: Option<u64>,
    /// Auto-compact threshold percent (0-100); defaults to 80 for BYOK/local models
    #[arg(long)]
    pub auto_compact_threshold_percent: Option<u8>,
    /// Default for this provider
    #[arg(long)]
    pub default: bool,
}

#[derive(Debug, Args, Clone)]
pub struct DiscoverArgs {
    #[arg(long)]
    pub ollama_url: Option<String>,
    #[arg(long)]
    pub lmstudio_url: Option<String>,
    #[arg(long)]
    pub add: bool,
}

#[derive(Debug, Args, Clone)]
pub struct ModelArgs {
    #[command(subcommand)]
    pub command: Option<ModelCommand>,
}

#[derive(Debug, Subcommand, Clone)]
pub enum ModelCommand {
    /// List available models
    List,
    /// Switch the default model
    Switch { model: String },
}

#[derive(Debug, Args, Clone)]
pub struct CronArgs {
    /// Cron expression (e.g. "0 9 * * *") or interval ("5m")
    pub expression: String,
    /// Prompt to run
    pub prompt: String,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    /// Run the job in yolo (non-interactive) mode
    #[arg(long)]
    pub yolo: bool,
}

#[derive(Debug, Args, Clone)]
pub struct ScheduleArgs {
    #[command(subcommand)]
    pub command: ScheduleCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum ScheduleCommand {
    /// List scheduled jobs
    List,
    /// Add a scheduled job
    Add(CronArgs),
    /// Delete a scheduled job
    Delete { name: String },
    /// Run a job now
    Run { name: String },
    /// Start the persistent scheduler daemon
    Start,
    /// Stop the persistent scheduler daemon
    Stop,
    /// Internal hidden command that runs the scheduler loop in the spawned daemon.
    #[command(hide = true)]
    Daemon,
}

#[derive(Debug, Args, Clone)]
pub struct TeamArgs {
    pub prompt: String,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(short, long, default_value = "2")]
    pub agents: usize,
    #[arg(long)]
    pub yolo: bool,
}

#[derive(Debug, Args, Clone)]
pub struct SwarmArgs {
    pub prompt: String,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(short, long, default_value = "3")]
    pub count: usize,
    #[arg(long)]
    pub yolo: bool,
}

#[derive(Debug, Args, Clone)]
pub struct SubagentArgs {
    #[command(subcommand)]
    pub command: SubagentCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum SubagentCommand {
    /// Spawn a subagent with a prompt
    Spawn {
        prompt: String,
        /// Run the subagent in yolo (non-interactive) mode
        #[arg(long)]
        yolo: bool,
    },
    /// List running subagents
    List,
    /// Kill a subagent by pid/name
    Kill { id: String },
    /// Show subagent logs
    Logs { id: String },
    /// Trace subagent execution
    Trace { id: String },
}

#[derive(Debug, Args, Clone)]
pub struct ResearchArgs {
    pub topic: String,
    #[arg(short, long, default_value = "5")]
    pub count: usize,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub struct TimelineArgs {
    #[arg(short, long, default_value = "20")]
    pub limit: usize,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone)]
pub struct HarnessArgs {
    #[command(subcommand)]
    pub command: HarnessCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum HarnessCommand {
    /// Add a harness connector
    Add {
        name: String,
        #[arg(value_enum)]
        r#type: HarnessType,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// API key / secret value to store (also reads OMGB_API_KEY env var if omitted)
        #[arg(long, env = "OMGB_API_KEY")]
        api_key: Option<String>,
        /// Environment variable name to expose the connector secret as in the child process
        /// (defaults to a per-type value such as OPENAI_API_KEY for codex).
        #[arg(long)]
        secret_env_key: Option<String>,
        /// Allow the connector URL to point to loopback/localhost
        #[arg(long)]
        allow_local: bool,
        /// Allow the connector URL to point to private/LAN addresses
        #[arg(long)]
        allow_private: bool,
    },
    /// List connectors
    List,
    /// Remove a connector
    Remove { name: String },
    /// Run a prompt through a connector
    Run { name: String, prompt: String },
}

#[derive(Debug, Clone, ValueEnum)]
pub enum HarnessType {
    Opencode,
    Codex,
    Claude,
    Hermes,
    Pi,
    Omp,
}

impl HarnessType {
    pub fn as_str(&self) -> &'static str {
        match self {
            HarnessType::Opencode => "opencode",
            HarnessType::Codex => "codex",
            HarnessType::Claude => "claude",
            HarnessType::Hermes => "hermes",
            HarnessType::Pi => "pi",
            HarnessType::Omp => "omp",
        }
    }
}

#[derive(Debug, Args, Clone)]
pub struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1:2419")]
    pub bind: SocketAddr,
    /// Host/IP to advertise in the QR pairing URL (defaults to a non-loopback local IP when binding 0.0.0.0/::)
    #[arg(long)]
    pub advertise_host: Option<IpAddr>,
    #[arg(long, env = "OMGB_AGENT_SECRET")]
    pub secret: Option<String>,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long)]
    pub yolo: bool,
    /// Comma-separated allowed Origin header values for WebSocket clients.
    /// Use `*` to allow any origin. If unset, no origin check is performed.
    #[arg(long, value_delimiter = ',')]
    pub allowed_origins: Vec<String>,
    /// Maximum WebSocket upgrade requests per minute per IP. 0 disables rate limiting.
    #[arg(long)]
    pub rate_limit: Option<u32>,
    /// Allow binding to a non-loopback address. Traffic will be unencrypted;
    /// use --allowed-origins to restrict client origins.
    #[arg(long)]
    pub insecure_allow_lan: bool,
}

#[derive(Debug, Args, Clone)]
pub struct ConnectArgs {
    pub url: String,
    #[arg(long, env = "OMGB_AGENT_SECRET")]
    pub secret: Option<String>,
    /// Allow connecting to private/LAN WebSocket targets
    #[arg(long)]
    pub allow_private: bool,
}

#[derive(Debug, Args, Clone)]
pub struct UseArgs {
    pub prompt: String,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long)]
    pub yolo: bool,
}

#[derive(Debug, Args, Clone)]
pub struct BrowserArgs {
    pub prompt: String,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long)]
    pub yolo: bool,
    #[arg(long)]
    pub url: Option<String>,
    /// Allow the starting URL to point to loopback/localhost
    #[arg(long)]
    pub allow_local: bool,
    /// Allow the starting URL to point to private/LAN addresses
    #[arg(long)]
    pub allow_private: bool,
}

#[derive(Debug, Args, Clone)]
pub struct TasteArgs {
    #[command(subcommand)]
    pub command: TasteCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum TasteCommand {
    /// Record something you like/prefer
    Like { note: String },
    /// Record something to avoid
    Dislike { note: String },
    /// List stored taste notes
    List,
}

#[derive(Debug, Args, Clone)]
pub struct CommitArgs {
    /// Commit message (defaults to "omgb commit")
    #[arg(short, long)]
    pub message: Option<String>,
    /// Stage and commit untracked files as well as tracked changes
    #[arg(long)]
    pub untracked: bool,
}

#[derive(Debug, Args, Clone)]
pub struct UndoArgs {
    /// Hard reset, discarding working tree changes
    #[arg(long)]
    pub hard: bool,
}
