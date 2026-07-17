# AGENTS.md — Oh My Grok Build

This repo is a fork of the open-source `xai-org/grok-build` SpaceXAI harness. We are building **oh-my-grok-build** (`omgb`): an opinionated productivity, orchestration, and mobile-relay layer on top of Grok Build.

## Project goals

- Keep Grok Build's Rust core untouched as the execution engine.
- Add the missing harness features found in `oh-my-codex`, `oh-my-pi`, `Command Code`, `Hermes`, etc.: BYOK providers, local model discovery, background/cron tasks, subagent orchestration, taste learning, team worktree isolation, git-native safety hooks, session branching helpers, and a mobile relay.
- Ship as a TypeScript/Node CLI (`tools/oh-my-grok-build`) plus a Grok plugin (`plugin/`) and a Capacitor React mobile app (`apps/mobile`).

## Repository layout

| Path | Purpose |
|------|---------|
| `crates/` | Upstream Grok Build Rust source (do not edit unless upstreaming) |
| `tools/oh-my-grok-build/` | `omgb` TypeScript harness: ACP relay, providers, team, sessions, exec, background scheduler, subagents |
| `plugin/` | Grok Build plugin: skills, hooks, agents, slash commands |
| `apps/mobile/` | Capacitor React mobile app (linked from `.vscode/vibe_app_slop` for local dev) |
| `AGENTS.md` | This file |

## Conventions

- Use **npm/Node** for JavaScript tooling (Bun may be added later).
- Prefer **TypeScript** with strict `tsconfig.json`.
- Keep code compact; avoid verbose error handling and unnecessary comments.
- Never log secrets or API keys.
- For Rust changes, follow existing `rustfmt.toml` and `clippy.toml`; run `cargo fmt` and `cargo clippy`.
- Run `npm run format:check` and `npm run format` (fix) before committing.
- Use Grok's extension points: plugins, skills, hooks, agents, MCP, ACP.
- Provider API keys are stored in `~/.omgb/.env`, never committed, and referenced by `env_key` in `~/.grok/config.toml`.

## Build & test

```bash
# TypeScript harness
cd tools/oh-my-grok-build
npm install
npm run typecheck
npm run build
npm test
npm run format:check

# Mobile app
cd apps/mobile
npm install
npm run typecheck
npm run build
npm run format:check
```

## Key principles

1. **Privacy-first / local-first**: never phone home. All relay traffic stays between the phone and the local machine.
2. **Grok-native**: use Grok's existing plugin, hook, skill, subagent, and MCP systems rather than reinventing them.
3. **Incremental**: small, composable tools. The CLI should be useful with one command (`omgb serve`) and grow from there.
4. **Industry-standard**: include tests, CI, type safety, clear docs, and secure credential handling.
