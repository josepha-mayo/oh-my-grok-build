# AGENTS.md — Oh My Grok Build

This repo is a fork of the open-source `xai-org/grok-build` SpaceXAI harness. We are building **oh-my-grok-build** (`omgb`): an opinionated productivity, orchestration, and mobile-relay layer on top of Grok Build.

## Project goals

- Keep Grok Build's Rust core untouched as the execution engine.
- Add the missing harness features found in `oh-my-codex`, `oh-my-pi`, `Command Code`, `Hermes`, etc.: persistent taste learning, team orchestration, git-native safety hooks, session branching helpers, and a mobile relay.
- Ship as a TypeScript/Bun CLI (`tools/oh-my-grok-build`) plus a Grok plugin (`plugin/`) and a mobile app.

## Repository layout

| Path | Purpose |
|------|---------|
| `crates/` | Upstream Grok Build Rust source (do not edit unless upstreaming) |
| `tools/oh-my-grok-build/` | `omgb` TypeScript harness: relay, taste, team, sessions, exec |
| `plugin/` | Grok Build plugin: skills, hooks, agents, slash commands |
| `mobile/` | Mobile web/PWA app (or moved to `.vscode/vibe_app_slop` after cleanup) |
| `AGENTS.md` | This file |

## Conventions

- Use **Bun** for JavaScript tooling in `tools/` and `mobile/`.
- Prefer **TypeScript** with strict `tsconfig.json`.
- Keep code compact; avoid verbose error handling and unnecessary comments.
- Never log secrets or API keys.
- For Rust changes, follow existing `rustfmt.toml` and `clippy.toml`; run `cargo fmt` and `cargo clippy`.

## Build & test

```bash
# Rust core (best-effort on Windows)
cargo check -p xai-grok-pager-bin

# TypeScript harness
cd tools/oh-my-grok-build
bun install
bun run build
bun test

# Mobile PWA
cd mobile
bun install
bun run build
```

## Key principles

1. **Privacy-first / local-first**: never phone home. All relay traffic stays between the phone and the local machine.
2. **Grok-native**: use Grok's existing plugin, hook, skill, subagent, and MCP systems rather than reinventing them.
3. **Incremental**: small, composable tools. The CLI should be useful with one command (`omgb serve`) and grow from there.
