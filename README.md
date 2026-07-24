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

## Quick start

Run a headless prompt:

```bash
omgb exec "explain this codebase"
```

Use a BYOK provider:

```bash
omgb provider add openai --api-key "$OPENAI_API_KEY" --default
omgb exec "write a rust fibonacci" --model omgb-openai
```

Run deep arXiv research (with optional model patch):

```bash
omgb research "quantum error correction" --count 5 --model omgb-openai
```

Start the WebSocket relay server:

```bash
omgb serve --bind 0.0.0.0:9999
omgb connect ws://127.0.0.1:9999
```

## Configuration

- `~/.grok/config.toml` — Grok Build configuration.
- `~/.omgb/config.json` — `omgb` provider and default model settings.
- `~/.omgb/.env` — provider API keys (never committed).
- `~/.omgb/schedule.jsonl` — background scheduled jobs.
- `~/.omgb/connectors.json` — cross-harness connector registry.

## Commands

| Command | Description |
| --- | --- |
| `omgb exec "<prompt>"` | Run a single headless turn. Use `--output-file` to capture stdout, `--yolo` to auto-approve tools. |
| `omgb tui` | Start the Grok pager UI. |
| `omgb provider add|list|remove|discover|test` | Manage BYOK/local providers and keys. |
| `omgb model switch <provider>` | Set the default model (provider id or `omgb-<id>`). |
| `omgb research "<topic>"` | Search arXiv and, if `--model` is given, generate a `.patch`. |
| `omgb loop "<prompt>"` | Iterate until the git working tree is clean (anti-loop guard). |
| `omgb swarm "<prompt>"` | Parallel subagents with task splitting and majority-vote fallback. |
| `omgb workflow run|list|show|new` | Run YAML/JSON workflows with exec/fan_out/shell steps. |
| `omgb subagent spawn|list|kill|logs|trace` | Spawn and manage child subagents with depth limits. |
| `omgb cron "<expr>" "<prompt>"` | Schedule a repeating job (cron or interval expression). |
| `omgb schedule list|run|delete|start|stop` | Manage scheduled jobs. |
| `omgb use` / `omgb browser` | Desktop/browser control. Gated by `--yolo` or `OMGB_ALLOW_DESKTOP_CONTROL=1`. |
| `omgb serve` / `omgb connect` | WebSocket relay server and client. |
| `omgb harness` | Register and run cross-harness connectors. |
| `omgb timeline` | Show recent session/job events. |

## Mobile app

A separate React Native + Expo mobile app lives in the `grok-build-app` repository.
It pairs with `omgb serve` over ACP/WebSocket using a QR code or manual URL/secret,
and supports chat, tool approval, model switching, slash commands, message history
paging, and a `/live` voice/text screen.

```bash
cd grok-build-app
npx expo start
```

## Security notes

- Provider API keys are written to `~/.omgb/.env` with `0600` permissions on Unix.
- Outgoing HTTP requests are pinned to resolved public IPs and redirects are disabled to mitigate SSRF.
- `omgb use` and `omgb browser` require explicit desktop-control gating.
- Shell commands passed through Grok's `run_terminal_cmd` are validated by `plugin/bin/safe-shell-guard`.

## Development

```bash
cargo fmt --check -p oh-my-grok-build
cargo clippy -p oh-my-grok-build
cargo test -p oh-my-grok-build
```

A release workflow in `.github/workflows/release.yml` builds `omgb` for Linux, macOS, and Windows on pushed `v*` tags.
