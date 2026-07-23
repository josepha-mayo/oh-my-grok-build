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
    /// List, resume, or fork persistent sessions
    #[command(subcommand)]
    Session(SessionCommand),
    /// Remember, recall, or manage persistent cross-session memory
    #[command(subcommand)]
    Memory(MemoryCommand),
    /// Apply hashline-anchored, token-efficient file patches
    #[command(subcommand)]
    Hashline(HashlineCommand),
    /// GitHub PR status / draft / merge-queue helpers
    #[command(subcommand)]
    Pr(PrCommand),
    /// List or start LSP language servers
    #[command(subcommand)]
    Lsp(LspCommand),
    /// List or start DAP debug adapters
    #[command(subcommand)]
    Dap(DapCommand),
    /// Browse and install plugins from the marketplace
    #[command(subcommand)]
    Plugin(PluginCommand),
    /// Run a deterministic CI playbook
    Playbook(PlaybookArgs),
    /// Computer use prompt
    Use(UseArgs),
    /// Browser use prompt
    Browser(BrowserArgs),
    /// Manage MCP servers
    Mcp(xai_grok_pager::mcp_cmd::McpArgs),
    /// Environment diagnostics and remediation
    Doctor(DoctorArgs),
    /// Remember a coding-style preference
    Taste(TasteArgs),
    /// Manage auto-generated skills
    Skill(SkillArgs),
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
    #[command(flatten)]
    pub session: SessionParams,
}

#[derive(Debug, Args, Clone)]
pub struct ExecArgs {
    pub prompt: Option<String>,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long, value_name = "FILE")]
    pub prompt_file: Option<PathBuf>,
    /// Internal: delete --prompt-file after the prompt is consumed.
    #[arg(long, hide = true)]
    pub prompt_file_own: bool,
    /// Auto-approve tool executions (required for non-interactive headless use).
    #[arg(long)]
    pub yolo: bool,
    #[arg(long)]
    pub json: bool,
    /// Comma-separated list of built-in tools to allow (e.g. read_file,grep,list_dir).
    #[arg(long, value_name = "TOOLS")]
    pub tools: Option<String>,
    /// Comma-separated list of built-in tools to disallow (e.g. run_terminal_cmd,search_replace).
    #[arg(long, value_name = "TOOLS")]
    pub disallowed_tools: Option<String>,
    #[arg(long)]
    pub output_file: Option<PathBuf>,
    /// Commit changes after the prompt finishes
    #[arg(long)]
    pub commit: bool,
    /// Stage and commit untracked files as well
    #[arg(long)]
    pub commit_untracked: bool,
    #[command(flatten)]
    pub session: SessionParams,
    /// Inject relevant cross-session memory into the prompt
    #[arg(long)]
    pub memory: bool,
    /// Maximum number of model turns for this prompt
    #[arg(long)]
    pub max_turns: Option<u32>,
}

#[derive(Debug, Args, Clone)]
pub struct LoopArgs {
    pub prompt: String,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(short = 'n', long, default_value = "50")]
    pub max_iterations: u32,
    /// Auto-approve tool use for the loop (required for non-interactive use).
    #[arg(long)]
    pub yolo: bool,
    /// Commit changes after the loop finishes (required for auto-commit)
    #[arg(long)]
    pub commit: bool,
    /// Stage and commit untracked files as well as tracked changes
    #[arg(long)]
    pub commit_untracked: bool,
    #[command(flatten)]
    pub session: SessionParams,
    /// Inject relevant cross-session memory into the prompt
    #[arg(long)]
    pub memory: bool,
    /// Maximum number of model turns per loop iteration
    #[arg(long)]
    pub max_turns: Option<u32>,
}

#[derive(Debug, Args, Clone)]
pub struct AutonomousArgs {
    pub prompt: String,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long, default_value = "workspace")]
    pub sandbox_profile: String,
    /// Auto-approve tool use for autonomous mode.
    #[arg(long)]
    pub yolo: bool,
    #[command(flatten)]
    pub session: SessionParams,
    /// Inject relevant cross-session memory into the prompt
    #[arg(long)]
    pub memory: bool,
    /// Maximum number of model turns for autonomous execution
    #[arg(long)]
    pub max_turns: Option<u32>,
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
    /// Auto-approve tool use for this scheduled job (required for non-interactive use).
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
    /// Set or clear a job's expiry time
    SetExpiry {
        name: String,
        #[arg(long)]
        expires_at: Option<String>,
    },
    /// Remove expired jobs from the schedule
    CleanupExpired,
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
    /// Auto-approve tool use for each team agent (required for non-interactive use).
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
    /// Auto-approve tool use for each swarm member (required for non-interactive use).
    #[arg(long)]
    pub yolo: bool,
    /// Use the original majority-vote ensemble instead of task splitting.
    #[arg(long)]
    pub ensemble: bool,
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
        /// Auto-approve tool use for the subagent (required for non-interactive use).
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
    /// Auto-approve read-only patch-generation tools (required when --model is used).
    #[arg(long)]
    pub yolo: bool,
    #[arg(long)]
    pub output: Option<PathBuf>,
}

/// Session-selection flags shared by commands that open a Grok session.
#[derive(Debug, Args, Clone, Default)]
pub struct SessionParams {
    /// Resume a session by ID, or the most recent if omitted.
    #[arg(
        long = "resume",
        short = 'r',
        value_name = "SESSION_ID",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub resume: Option<String>,
    /// Continue the most recent session for this workspace.
    #[arg(long = "continue", short = 'c')]
    pub continue_last: bool,
    /// Use a specific session ID for a new or forked session.
    #[arg(long = "session-id", short = 's', value_name = "SESSION_ID")]
    pub session_id: Option<String>,
    /// When resuming or continuing, fork to a new session instead of reusing.
    #[arg(long = "fork-session")]
    pub fork_session: bool,
}

#[derive(Debug, Subcommand, Clone)]
pub enum SessionCommand {
    /// List persisted sessions for the current workspace
    List,
    /// Resume a session (or the most recent) and continue a prompt
    Resume(SessionResumeArgs),
    /// Fork a session into a new branch
    Fork(SessionForkArgs),
    /// Start a fresh named session
    New(SessionNewArgs),
}

#[derive(Debug, Args, Clone)]
pub struct SessionResumeArgs {
    /// Session ID to resume, or omit to resume the most recent.
    pub source_session_id: Option<String>,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long)]
    pub yolo: bool,
    /// Inject relevant cross-session memory into the prompt
    #[arg(long)]
    pub memory: bool,
    #[arg(long = "continue", short = 'c')]
    pub continue_last: bool,
    #[arg(long = "fork-session")]
    pub fork_session: bool,
    /// New session ID when forking (requires --fork-session).
    #[arg(long = "session-id", short = 's', value_name = "SESSION_ID")]
    pub target_session_id: Option<String>,
    /// Optional follow-up prompt (omit for an empty turn / TUI-less resume).
    pub prompt: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct SessionForkArgs {
    /// Parent session ID to fork from.
    pub parent_session_id: String,
    /// New session ID for the fork.
    #[arg(long = "session-id", short = 's', value_name = "SESSION_ID")]
    pub new_session_id: Option<String>,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long)]
    pub yolo: bool,
    /// Inject relevant cross-session memory into the prompt
    #[arg(long)]
    pub memory: bool,
    /// Optional follow-up prompt.
    pub prompt: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct SessionNewArgs {
    #[arg(long = "session-id", short = 's', value_name = "SESSION_ID")]
    pub session_id: Option<String>,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long)]
    pub yolo: bool,
    /// Inject relevant cross-session memory into the prompt
    #[arg(long)]
    pub memory: bool,
    /// Initial prompt.
    pub prompt: String,
}

#[derive(Debug, Subcommand, Clone)]
pub enum MemoryCommand {
    /// Store a note in persistent memory
    Remember(MemoryRememberArgs),
    /// Search memory by keyword
    Recall(MemoryRecallArgs),
    /// List recent memory notes
    List(MemoryListArgs),
    /// Record a one-shot occurrence journal entry
    Oneshot(MemoryOneshotArgs),
    /// Deduplicate near-duplicate notes
    Compact,
}

#[derive(Debug, Args, Clone)]
pub struct MemoryRememberArgs {
    /// The note to remember
    pub content: String,
    #[arg(short, long, value_delimiter = ',')]
    pub tags: Vec<String>,
}

#[derive(Debug, Args, Clone)]
pub struct MemoryRecallArgs {
    pub query: String,
    #[arg(short, long, default_value = "5")]
    pub limit: usize,
}

#[derive(Debug, Args, Clone)]
pub struct MemoryListArgs {
    #[arg(short, long)]
    pub tag: Option<String>,
    #[arg(short, long, default_value = "20")]
    pub limit: usize,
}

#[derive(Debug, Args, Clone)]
pub struct MemoryOneshotArgs {
    /// Short topic/category for the one-shot note
    pub topic: String,
    /// Detailed note text
    pub detail: String,
}

#[derive(Debug, Subcommand, Clone)]
pub enum HashlineCommand {
    /// Apply a hashline patch file to a target file
    Apply(HashlineApplyArgs),
    /// Verify a hashline patch without writing changes
    Verify(HashlineApplyArgs),
}

#[derive(Debug, Args, Clone)]
pub struct HashlineApplyArgs {
    pub file: std::path::PathBuf,
    /// Patch file path, or `-` to read from stdin
    #[arg(short, long, default_value = "-")]
    pub patch: String,
}

#[derive(Debug, Subcommand, Clone)]
pub enum PrCommand {
    /// Show PR status for the current (or specified) branch
    Status(PrStatusArgs),
    /// Create a PR (use --draft to create as draft)
    Create(PrCreateArgs),
    /// Create a draft PR (deprecated; use `create --draft`)
    CreateDraft(PrCreateArgs),
    /// Update an existing PR's title and body
    Update(PrUpdateArgs),
    /// Merge the PR for this branch
    Merge(PrMergeArgs),
    /// Request reviewers for the PR on this branch
    Review(PrReviewArgs),
    /// Check whether the branch's PR is in a merge queue
    MergeQueue(PrStatusArgs),
    /// Show CI checks for the current (or specified) branch
    Checks(PrStatusArgs),
}

#[derive(Debug, Args, Clone)]
pub struct PrStatusArgs {
    #[arg(short, long)]
    pub branch: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct PrCreateArgs {
    #[arg(short, long)]
    pub title: Option<String>,
    #[arg(short, long, default_value = "Generated with omgb")]
    pub body: String,
    /// Target base branch (auto-detected if omitted)
    #[arg(short, long)]
    pub base: Option<String>,
    /// Labels to apply (repeatable)
    #[arg(short, long)]
    pub label: Vec<String>,
    /// Reviewers to request (repeatable)
    #[arg(short, long)]
    pub reviewer: Vec<String>,
    /// Auto-generate title/body from the latest commit on this branch
    #[arg(long)]
    pub fill: bool,
    /// Create the PR as a draft
    #[arg(long)]
    pub draft: bool,
}

#[derive(Debug, Args, Clone)]
pub struct PrUpdateArgs {
    #[arg(short, long)]
    pub title: Option<String>,
    #[arg(short, long)]
    pub body: Option<String>,
    #[arg(short, long)]
    pub branch: Option<String>,
    /// Labels to add (repeatable)
    #[arg(short, long)]
    pub label: Vec<String>,
    /// Reviewers to add (repeatable)
    #[arg(short, long)]
    pub reviewer: Vec<String>,
}

#[derive(Debug, Args, Clone)]
pub struct PrMergeArgs {
    #[arg(short, long)]
    pub branch: Option<String>,
    #[arg(long, default_value = "squash")]
    pub method: String,
}

#[derive(Debug, Args, Clone)]
pub struct PrReviewArgs {
    #[arg(short, long)]
    pub branch: Option<String>,
    #[arg(required = true)]
    pub reviewers: Vec<String>,
}

#[derive(Debug, Subcommand, Clone)]
pub enum LspCommand {
    /// List known LSP servers
    List,
    /// Start an LSP server in the workspace
    Start(LspStartArgs),
    /// Run a semantic rename/refactor on a file
    Refactor {
        file: std::path::PathBuf,
        old_name: String,
        new_name: String,
    },
}

#[derive(Debug, Args, Clone)]
pub struct LspStartArgs {
    pub server: String,
    #[arg(short, long, default_value = ".")]
    pub cwd: std::path::PathBuf,
    #[arg(short, long, value_delimiter = ',')]
    pub languages: Vec<String>,
}

#[derive(Debug, Subcommand, Clone)]
pub enum DapCommand {
    /// List known debug adapters
    List,
    /// Start a debug adapter
    Start(DapStartArgs),
    /// Attach a DAP adapter to a running process
    Attach {
        program: std::path::PathBuf,
        #[arg(long)]
        pid: u32,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
}

#[derive(Debug, Args, Clone)]
pub struct DapStartArgs {
    pub adapter: String,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub extra: Vec<String>,
}

#[derive(Debug, Subcommand, Clone)]
pub enum PluginCommand {
    /// List installed plugins
    List,
    /// Install a plugin from a git URL or local directory
    Install(PluginInstallArgs),
    /// Remove an installed plugin by name
    Remove { name: String },
    /// Refresh installed plugins from their git sources
    Refresh(PluginRefreshArgs),
}

#[derive(Debug, Args, Clone)]
pub struct PluginInstallArgs {
    /// Git URL or local directory path to the plugin
    pub source: String,
    /// Optional plugin name (derived from URL if omitted)
    #[arg(short, long)]
    pub name: Option<String>,
    /// Optional SHA to pin the remote plugin to
    #[arg(long = "sha")]
    pub require_sha: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct PluginRefreshArgs {
    /// Plugin name to refresh (refresh all if omitted)
    pub name: Option<String>,
    /// Non-blocking refresh: do not wait for each source
    #[arg(long)]
    pub async_refresh: bool,
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
    /// Record an output you accepted as correct
    Accept {
        prompt: String,
        output: String,
        #[arg(short, long, value_delimiter = ',')]
        tags: Vec<String>,
    },
    /// Record an output you rejected
    Reject {
        prompt: String,
        output: String,
        #[arg(short, long, value_delimiter = ',')]
        tags: Vec<String>,
    },
    /// Record a before/after edit you preferred
    Edit {
        prompt: String,
        before: String,
        after: String,
        #[arg(short, long, value_delimiter = ',')]
        tags: Vec<String>,
    },
    /// List stored taste notes
    List,
}

#[derive(Debug, Args, Clone)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub command: SkillCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum SkillCommand {
    /// List persisted skills
    List,
    /// Show a skill by name
    Show { name: String },
    /// Suggest and write a skill from the timeline
    AutoCreate {
        #[arg(short, long, default_value = "5")]
        threshold: usize,
    },
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

#[derive(Debug, Args, Clone)]
pub struct DoctorArgs {
    /// Apply safe remediations
    #[arg(long)]
    pub fix: bool,
    /// Print report as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone)]
pub struct PlaybookArgs {
    /// Playbook file (TOML or JSON)
    pub file: PathBuf,
    /// Print the steps without running them
    #[arg(long)]
    pub dry_run: bool,
}
