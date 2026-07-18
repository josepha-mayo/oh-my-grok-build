# oh-my-grok-build CLI (`omgb`)

A productivity and orchestration layer for [Grok Build](https://github.com/xai-org/grok-build).

## Features

- **ACP relay**: `omgb serve` starts a WebSocket bridge to `grok agent stdio`, generates a secret, and prints a QR code for the mobile app.
- **BYOK providers**: `omgb provider add` wires OpenAI, Anthropic, xAI, OpenRouter, Ollama, LM Studio, and custom OpenAI-compatible endpoints into `~/.grok/config.toml` while keeping API keys in `~/.omgb/.env`.
- **Local model discovery**: `omgb provider discover` finds Ollama/LM Studio models automatically.
- **Interactive ACP client**: `omgb connect <url>` lets you chat with a running agent server from the terminal.
- **Headless execution**: `omgb exec <prompt>` and `omgb team <count> <prompt>`.
- **Autonomous loops**: `omgb loop` iterates until the working tree is clean, and `omgb cron`/`omgb schedule` run prompts on a schedule.
- **Computer & browser use**: `omgb use <prompt>` controls the desktop via the `omgb-computer` MCP server (and can use browser tools), while `omgb browser <prompt>` drives the `omgb-browser` MCP server.
- **Research**: `omgb research <topic>` searches arXiv, synthesizes a report, and proposes a patch.
- **Subagent orchestration**: `omgb subagent spawn/list/kill/logs/trace` and `omgb swarm` parallelize work across subagents.
- **Timeline**: `omgb timeline` shows recent session/job events.
- **Cross-harness connector**: drive OpenCode, Codex, Claude, Hermes, and other CLI agents from `omgb`.

## Install

```bash
cd tools/oh-my-grok-build
npm install
npm run build
npm link   # or use ./dist/index.js directly
```

## Quick start

```bash
# Start the agent server with a QR code for mobile pairing
omgb serve

# Add a BYOK provider
omgb provider add openai

# Run a headless task
omgb exec "refactor the auth module"

# Use the desktop/browser (requires OMGB_ALLOW_DESKTOP_CONTROL=1 for computer use)
omgb use "open Notepad and type hello"
omgb browser "go to arxiv.org and search for grok"

# Research an arXiv topic
omgb research "mechanistic interpretability"

# View recent activity
omgb timeline

# Connect as a CLI client
omgb connect ws://host:port/ws?server-key=...
```

## Security notes

- API keys are stored in `~/.omgb/.env` and referenced by `env_key` in `~/.grok/config.toml`; they are never written to the Grok config.
- The `omgb serve` QR code and URL contain the `server-key` secret; treat them like a password and do not share them.
- Saved mobile connections strip the `server-key` query parameter; re-scan or re-enter the full pairing URL when reconnecting.

## Development

```bash
npm install
npm run typecheck
npm run build
npm test
npm run format:check
```
