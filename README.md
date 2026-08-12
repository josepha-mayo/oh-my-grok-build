<div align="center">

# oh my grok build

An opinionated productivity, orchestration, and mobile-relay layer on top of Grok Build.

</div>

---

## Installation

`omgb` is built as a Rust binary in the workspace:

```bash
cargo build -p oh-my-grok-build --release
```

The binary is produced at `target/release/omgb` (or `target\release\omgb.exe` on Windows).

Distribution builds use the hardened profile:

```bash
cargo build -p oh-my-grok-build --profile release-dist
```

Published releases also include verified first-install scripts. Download the
script from the same release you intend to install, inspect it, then run it:

```bash
# Linux or macOS
sh install.sh vMAJOR.MINOR.PATCH
```

```powershell
# Windows PowerShell
.\install.ps1 -Version vMAJOR.MINOR.PATCH
```

The installers verify the release archive's SHA-256 checksum and GitHub build
provenance by default, install the adjacent plugin tree, and run `omgb doctor`.
They require the GitHub CLI (`gh`) unless their explicit insecure override is
used.

## Quick start

Run a headless prompt:

```bash
omgb exec "explain this codebase"
```

Use a BYOK provider:

```bash
omgb provider catalog
OMGB_API_KEY="$OPENAI_API_KEY" omgb provider add openai --default
omgb exec "write a rust fibonacci" --model omgb-openai
```

Or discover a local provider without any account sign-in:

```bash
omgb provider discover --add
```

Grok subscription sign-in is optional and separate from BYOK/local setup:

```bash
omgb auth status
omgb auth login            # device code
omgb auth login --browser  # local browser callback
```

Run deep arXiv research (with optional model patch):

```bash
omgb research "quantum error correction" --count 5 --model omgb-openai --yolo
```

Start the WebSocket relay server:

```bash
omgb serve --bind 0.0.0.0:9999 --insecure-allow-lan --allowed-origins '*'
omgb connect ws://127.0.0.1:9999 --secret <pairing-secret>
```

LAN mode is explicitly opt-in. Prefer `wss://` behind a TLS-terminating proxy
when it is available, and replace `*` with exact browser origins when serving
browser clients. The same `--allowed-origins` policy controls WebSocket origin
checks and CORS for the authenticated HTTP group API. An HTTPS-hosted web
companion requires `wss://`; use the native app for a local `ws://` relay.
The secret-free `GET /healthz` endpoint is available for local liveness checks.
The relay exits if its embedded ACP agent stops, so a service manager does not
keep advertising a listener that cannot accept sessions.

## Configuration

- `~/.grok/config.toml` — Grok Build configuration.
- `~/.omgb/config.json` — `omgb` provider and default model settings.
- `~/.omgb/.env` — provider API keys (never committed).
- `~/.omgb/schedule.jsonl` — background scheduled jobs.
- `~/.omgb/connectors.json` — cross-harness connector registry.

## Commands

| Command | Description |
| --- | --- |
| `omgb` / `omgb tui` | Start the Grok pager UI (default when no subcommand is given). |
| `omgb exec "<prompt>"` | Run a single headless turn. Use `--output-file` to capture stdout, `--yolo` to auto-approve tools. |
| `omgb loop "<prompt>"` | Iterate until the git working tree is clean (anti-loop guard). |
| `omgb autonomous "<prompt>"` | High-autonomy mode with guard checks and auto-approval. |
| `omgb provider list|catalog|add|remove|discover|test` | Manage BYOK/local provider templates and keys. |
| `omgb auth status|login|logout` | Manage optional Grok subscription sign-in without changing BYOK keys. |
| `omgb model list` / `omgb model switch <model>` | List models or set the default model (provider id or `omgb-<id>`). |
| `omgb cron "<expr>" "<prompt>"` | Schedule a repeating job (cron or interval expression). |
| `omgb schedule list|add|run|delete|start|stop|set-expiry|cleanup-expired` | Manage scheduled jobs. |
| `omgb team` | Team mode with isolated git worktrees. |
| `omgb swarm "<prompt>"` | Parallel subagent swarm with task splitting and majority-vote fallback. |
| `omgb subagent spawn|list|kill|logs|trace` | Spawn and manage child subagents with depth limits. |
| `omgb thread` | Multi-agent thread orchestration across sessions. |
| `omgb meta` | Meta-harness autonomous planning, execution, and learning. |
| `omgb research "<topic>"` | Search arXiv and, if `--model` is given, generate a `.patch`. |
| `omgb timeline` | Show recent session/job events. |
| `omgb harness` | Register and run cross-harness connectors (Codex, Claude, OpenCode, Hermes, Pi, OMP). |
| `omgb serve` | Start the ACP WebSocket relay for the mobile app. |
| `omgb connect <url>` | Connect to an ACP relay. |
| `omgb session` | List, resume, or fork persistent sessions. |
| `omgb memory` | Remember, recall, or manage persistent cross-session memory. |
| `omgb hashline` | Apply hashline-anchored, token-efficient file patches. |
| `omgb pr status|create|update|merge|checks|merge-queue` | GitHub PR helpers. |
| `omgb lsp` | List or start LSP language servers. |
| `omgb dap` | List or start DAP debug adapters. |
| `omgb plugin list|install|uninstall` | Browse and install plugins from the marketplace. |
| `omgb playbook` | Run deterministic CI playbooks. |
| `omgb workflow run|list|show|new|create` | Run YAML/JSON workflows with exec/fan_out/shell steps. |
| `omgb group` | Multi-agent group chat with humans and agents. |
| `omgb use` / `omgb browser` | Computer / browser use (gated by `--yolo` or `OMGB_ALLOW_DESKTOP_CONTROL=1`). |
| `omgb mcp` | Manage MCP servers. |
| `omgb doctor` | Environment diagnostics and remediation. |
| `omgb update --check` / `omgb update --apply` | Check for or install an attestation-verified GitHub Release update. |
| `omgb taste` | Remember a coding-style preference. |
| `omgb skill` | Manage auto-generated skills. |
| `omgb commit` | Commit the current working tree. |
| `omgb review` | Review current changes (git status + diff). |
| `omgb undo` | Undo the last omgb commit. |
| `omgb feedback "<message>"` | Open a GitHub issue to submit feedback (`--open` to launch browser). |

## Mobile app

A separate React Native + Expo mobile app lives in the `grok-build-app` repository. The `omgb serve` QR includes the harness's absolute working directory so the app can create a valid ACP session on both Windows and Unix without guessing a path.
It pairs with `omgb serve` over ACP/WebSocket using a QR code or manual URL/secret,
and supports chat, tool approval, model switching, slash commands, `/help` for command
discovery, message history paging, and a `/live` hold-to-talk dictation screen. Live
audio uses the same pairing secret on the relay's `/voice` endpoint; the harness keeps
the xAI bearer local and returns STT transcripts to the phone, which submits each final
transcript through its active ACP session.

```bash
cd grok-build-app
npx expo start
```

## Security & privacy notes

- Provider API keys are written to `~/.omgb/.env` with `0600` permissions on Unix.
- Outgoing HTTP requests are pinned to resolved public IPs and redirects are disabled to mitigate SSRF.
- `omgb use` and `omgb browser` require explicit desktop-control gating (`--yolo` and `OMGB_ALLOW_DESKTOP_CONTROL=1`).
- Shell commands passed through Grok's `run_terminal_cmd` are validated by `plugin/bin/safe-shell-guard`.
- Telemetry and upstream feedback are disabled by default; use `omgb feedback` to submit issues via GitHub.

## Development

```bash
cargo fmt --check -p oh-my-grok-build
cargo clippy -p oh-my-grok-build
cargo test -p oh-my-grok-build
```

A release workflow in `.github/workflows/release.yml` builds `omgb` for Linux x86_64/ARM64, macOS Intel/Apple Silicon, and Windows x86_64 on pushed `v*` tags. Each release artifact is accompanied by a checksum, SBOM, and GitHub build-provenance attestation; the release also includes AMD64/ARM64 Debian and RPM packages, an x64 MSI, plus Unix and Windows first-install scripts.

Maintainers should follow the [release playbook](docs/releasing.md) to verify the generated draft before making it available to self-updating clients.
