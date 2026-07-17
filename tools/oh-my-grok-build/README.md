# oh-my-grok-build CLI (`omgb`)

A productivity and orchestration layer for [Grok Build](https://github.com/xai-org/grok-build).

## Features

- **ACP relay**: `omgb serve` starts `grok agent serve`, generates a secret, and prints a QR code for the mobile app.
- **BYOK providers**: `omgb provider add` wires OpenAI, Anthropic, xAI, OpenRouter, Ollama, LM Studio, and custom OpenAI-compatible endpoints into `~/.grok/config.toml`.
- **Local model discovery**: `omgb provider discover` finds Ollama/LM Studio models automatically.
- **Interactive ACP client**: `omgb connect <url>` lets you chat with a running agent server from the terminal.
- **Headless execution**: `omgb exec <prompt>` and `omgb team <count> <prompt>`.
- **Background tasks**: `omgb loop` and `omgb schedule` for cron-style runs.
- **Subagent orchestration**: `omgb subagent spawn/list/kill/logs`.
- **Cross-harness connector**: (in progress) drive OpenCode, Codex, and Claude CLI from `omgb`.

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

# Connect as a CLI client
omgb connect ws://host:port/ws?server-key=...
```

## Development

```bash
npm install
npm run typecheck
npm run build
npm test
```
