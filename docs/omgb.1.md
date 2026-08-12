# OMBG(1) -- Oh My Grok Build CLI

## NAME

`omgb` - opinionated productivity, orchestration, and mobile-relay layer for Grok Build

## SYNOPSIS

`omgb` [<i>OPTIONS</i>] [<i>COMMAND</i>] [<i>ARGS</i>]<br>
`omgb tui` [<i>PROMPT</i>]<br>
`omgb exec` [<i>OPTIONS</i>] [<i>PROMPT</i>]<br>
`omgb serve` [<i>OPTIONS</i>]<br>
`omgb update` [<i>OPTIONS</i>]<br>
`omgb doctor`<br>
`omgb` `--version`<br>
`omgb` `--help`

## DESCRIPTION

`omgb` wraps Grok Build's Rust execution engine with a CLI, a mobile WebSocket relay, subagent orchestration, local model fallbacks, scheduled jobs, team worktrees, and reusable workflows.

When called without a command, `omgb` starts the interactive TUI.

## OPTIONS

- `-h`, `--help`
  Print help information.

- `-V`, `--version`
  Print version information.

## COMMANDS

- `tui`
  Run the interactive TUI.

- `exec`
  Execute a single prompt and exit.

- `loop`
  Run an autonomous diff-driven work loop.

- `autonomous`
  High-autonomy mode (same as `exec --yolo` with guard checks).

- `provider`
  Manage BYOK and local model providers.

- `auth`
  Inspect, create, or clear the optional Grok subscription session. Login uses
  a device code unless `--browser` is passed and never changes BYOK keys.

- `model`
  List or switch models.

- `cron`, `schedule`
  Schedule and manage background jobs.

- `team`
  Manage isolated git worktrees for team members.

- `swarm`
  Run parallel subagents.

- `subagent`
  Spawn, list, kill, and inspect subagents.

- `thread`
  Multi-agent thread orchestration with persistent, bounded peer inboxes.

- `research`
  Deep arXiv / web research.

- `serve`
  Start the ACP/WebSocket relay for the mobile app.

- `connect`
  Connect to an ACP relay.

- `session`, `memory`
  Manage persistent sessions and cross-session memory.

- `hashline`
  Apply token-efficient file patches.

- `pr`
  GitHub PR helpers.

- `lsp`, `dap`
  Start language-server or debug-adapter integrations.

- `plugin`
  Browse and install plugins from the marketplace.

- `playbook`, `workflow`, `group`
  Run playbooks, workflows, and multi-agent group chat.

- `use`, `browser`
  Computer-use and browser-use prompts.

- `mcp`
  Manage MCP servers.

- `doctor`
  Run environment diagnostics.

- `taste`
  Record a coding-style preference.

- `skill`
  Manage auto-generated skills.

- `commit`, `review`, `undo`
  Git helpers.

- `update`
  Check for and install self-updates from GitHub Releases. By default this verifies the build-provenance attestation using the GitHub CLI (`gh`). Use `--insecure` only when `gh` is unavailable (not recommended).

- `feedback`
  Submit feedback or file an issue.

## ENVIRONMENT

- `PROTOC`
  Path to the Protocol Buffers compiler. If unset, `protoc` must be on `PATH`.

- `GROK_HOME`
  Override the Grok Build configuration directory.

- `OMGB_HOME`
  Override the `omgb` data directory (`~/.omgb` by default).

## FILES

- `~/.grok/config.toml`
  Grok Build profile, model, and endpoint settings.

- `~/.omgb/config.json`
  `omgb` provider and default model settings.

- `~/.omgb/.env`
  API keys and connector secrets. Must have `0600` permissions.

- `~/.omgb/schedule.jsonl`
  Background scheduled jobs.

- `~/.omgb/connectors.json`
  Connector registry without secrets.

## SAFETY

Never commit `~/.omgb/.env`. Keep the pairing secret from `omgb serve` private. See `SECURITY.md` and `PRIVACY.md`.

## SEE ALSO

`SECURITY.md`, `PRIVACY.md`, `FEATURES.md`, `docs/user-guide.md`
