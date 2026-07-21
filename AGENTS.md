# AGENTS.md — Oh My Grok Build

This repo adds **oh-my-grok-build** (`omgb`) as an opinionated productivity, orchestration, and mobile-relay layer on top of the open-source `xai-org/grok-build` Rust core.

## Project goals

- Reuse Grok Build's Rust crates as the execution engine and extend them with `omgb` commands.
- Add the missing harness features found in `oh-my-codex`, `oh-my-pi`, `Command Code`, `Hermes`, etc.: BYOK providers, local model discovery, background/cron tasks, subagent orchestration, taste learning, team worktree isolation, git-native safety hooks, and a mobile relay.
- Ship the `omgb` CLI as a Rust binary in the `oh-my-grok-build` crate, controlled by a Grok Build plugin (`plugin/`).

## Repository layout

| Path | Purpose |
|------|---------|
| `crates/codegen/` | Upstream Grok Build Rust source (treat as upstream; edit only when necessary) |
| `crates/oh-my-grok-build/` | `omgb` Rust CLI: providers, scheduler, subagents, team mode, research, server/relay, connectors |
| `plugin/` | Grok Build plugin: skills, hooks, agents, slash commands (incl. `/use`, `/browser`, `/schedule`, `/btw`) |
| `grok-build-app/` | Native mobile app (planned; archived Capacitor prototype) |
| `AGENTS.md` | This file |

## Conventions

- This is a **Rust-first** workspace; all production code lives in the Rust crate.
- Run `cargo fmt -p oh-my-grok-build` and `cargo clippy -p oh-my-grok-build` before committing.
- Keep code compact; avoid verbose error handling and unnecessary comments.
- Never log secrets or API keys.
- Provider API keys are stored in `~/.omgb/.env` (Unix permissions `0600`), never committed, and referenced by `env_key` in `~/.grok/config.toml`.
- Connector secrets are stored the same way; `connectors.json` only keeps the env-key reference.
- WebSocket and HTTP URLs are validated against private/metadata hosts.

## Configuration

- `~/.grok/config.toml` — Grok Build configuration (sandbox profile, model, endpoints).
- `~/.omgb/config.json` — `omgb` provider and default model settings.
- `~/.omgb/.env` — API keys and connector secrets (never committed).
- `~/.omgb/schedule.jsonl` — background scheduled jobs.
- `~/.omgb/connectors.json` — cross-harness connector registry (no secrets).
- `~/.omgb/subagents.jsonl` — subagent registry.

## Build & test

```bash
cargo fmt -p oh-my-grok-build
cargo clippy -p oh-my-grok-build
cargo test -p oh-my-grok-build
node --test plugin/bin/safe-shell-guard.test.js
```

The binary is produced at `target/release/omgb` (`target\release\omgb.exe` on Windows).

### Build dependencies

- `protoc` (v29.3) must be on `PATH` (or set via `$PROTOC`). CI installs it via `arduino/setup-protoc@v3`.
- On Windows with the `x86_64-pc-windows-gnu` target, a MinGW-w64 toolchain (e.g. WinLibs UCRT) must be on `PATH` so `cc` can find `gcc.exe`. Visual Studio Build Tools provide `cl.exe` for the MSVC target.
- If your rustup default host is MSVC but you want to use the MinGW toolchain, run `rustup override set 1.92.0-x86_64-pc-windows-gnu` in the repo so `cargo` does not look for `link.exe`.

## Key principles

1. **Privacy-first / local-first**: relay traffic stays between the phone and the local machine.
2. **Grok-native**: use Grok's existing plugin, hook, skill, subagent, and MCP systems rather than reinventing them.
3. **Incremental**: small, composable tools. The CLI should be useful with one command (`omgb serve`) and grow from there.
4. **Industry-standard**: include tests, CI, type safety, clear docs, and secure credential handling.
