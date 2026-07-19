# AGENTS.md — Oh My Grok Build

This repo is a fork of the open-source `xai-org/grok-build` harness. We are building **oh-my-grok-build** (`omgb`): an opinionated productivity, orchestration, and mobile-relay layer on top of the existing Rust harness.

## Project goals

- Extend the Grok Build **Rust harness** (`crates/`) instead of writing a parallel tool in TypeScript/Bun/Python.
- Add the missing harness features found in `oh-my-codex`, `oh-my-pi`, `Command Code`, `Hermes`, etc.: BYOK providers, local model discovery, background/cron tasks, subagent orchestration, taste learning, team worktree isolation, git-native safety hooks, session branching helpers, research, timeline, cross-harness connectors, and a mobile relay.
- Ship a single Rust binary named `oh-my-grok-build` (alias `omgb`) that reuses upstream crates and registers new subcommands/slash commands.
- Keep edits to upstream `xai-grok-*` crates minimal and upstream-friendly. Prefer new `omgb-*` crates and upstream public APIs; when a seam is missing, expose it upstream rather than forking logic.
- The mobile app is a separate, real native project and is **not** in this harness repo. It talks to `omgb serve` over ACP/WebSocket.

## Repository layout

| Path | Purpose |
|------|---------|
| `crates/codegen/xai-grok-*` | Upstream Grok Build Rust source. Edit sparingly, mark changes clearly. |
| `crates/oh-my-grok-build` | Composition-root binary crate for `oh-my-grok-build` / `omgb`. |
| `crates/omgb-*` | New Rust crates for providers, scheduler, subagents, taste, timeline, research, harness connectors, mobile relay, etc. |
| `plugin/` | Grok Build plugin skills and slash commands (`/use`, `/browser`, `/schedule`, `/loop`, `/btw`, `/taste`, `/autonomous`, `/research`, etc.). |
| `AGENTS.md` | This file. |
| `FEATURES.md` | Single source of truth for features, goals, and roadmap. |

## Conventions

- Use **Rust** for all new harness code. Cargo workspace conventions apply.
- Follow existing `rustfmt.toml`, `clippy.toml`, and crate naming patterns.
- Keep code compact; avoid verbose error handling and unnecessary comments.
- Never log secrets or API keys.
- Provider API keys are stored in `~/.omgb/.env`, never committed, and referenced by `env_key` in `~/.grok/config.toml`.
- Connector and MCP `env` maps are filtered to `*_API_KEY` keys only; dangerous keys such as `PATH` or `LD_PRELOAD` cannot be injected.
- WebSocket and browser URLs are validated against private/metadata hosts.
- Run `cargo fmt --check`, `cargo clippy --workspace`, and `cargo test --workspace` before committing.
- Use Grok's extension points: plugins, skills, hooks, agents, MCP, ACP.

## Build & test

```bash
# Format/lint/test the Rust workspace
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace

# Distribution build
cargo build --bin oh-my-grok-build --profile release-dist
```

## Key principles

1. **Harness, not engine.** The upstream Rust core is the harness; we build on top of it.
2. **Privacy-first / local-first.** No telemetry unless explicitly opted in. Relay traffic stays between the phone and the local machine.
3. **Grok-native.** Use Grok's existing plugin, hook, skill, subagent, and MCP systems rather than reinventing them.
4. **Incremental.** Small, composable crates. The CLI should be useful with one command (`omgb serve`) and grow from there.
5. **Industry-standard.** Tests, CI, type safety, clear docs, and secure credential handling for every feature.

## Important

- Do **not** recreate the Node/TypeScript harness or the Capacitor web mobile app in this repo.
- `FEATURES.md` is the source of truth for scope and status; update it as work progresses.
