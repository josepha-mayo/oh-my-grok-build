# Grok Build Feature Checklist

This file tracks the features requested for the `oh-my-grok-build` harness and their current status.

`oh-my-grok-build` (`omgb`) is an opinionated productivity, orchestration, and mobile-relay layer on top of the open-source `xai-org/grok-build` harness. It keeps the Rust core untouched, adds the missing harness features found in tools like `oh-my-codex`, `oh-my-pi`, `Command Code`, `Hermes`, etc., and is built privacy-first / local-first.

## Project goals

- Keep Grok Build's Rust core untouched as the execution engine.
- Add the missing harness features found in `oh-my-codex`, `oh-my-pi`, `Command Code`, `Hermes`, etc.: BYOK providers, local model discovery, background/cron tasks, subagent orchestration, taste learning, team worktree isolation, git-native safety hooks, session branching helpers, and a mobile relay.
- Ship as a TypeScript/Node CLI (`tools/oh-my-grok-build`) plus a Grok plugin (`plugin/`) and a Capacitor React mobile app (`grok-build-app/`).
- Privacy-first / local-first: never phone home. All relay traffic stays between the phone and the local machine.
- Build incrementally: the CLI should be useful with one command (`omgb serve`) and grow from there.

## Status key

- `✅` = verified (implemented + tested + runtime verified)
- `🚧` = implemented but not yet fully verified
- `⏳` = planned / not yet started
- `N/A` = not applicable for this phase

A feature is only `✅` when it is implemented, covered by tests, and the tests/verification have actually been run and passed. Build/typecheck alone is not enough.

## Phase 1 — Core harness (current implementation)

| Feature | Implemented | Tested | Working |
| --- | --- | --- | --- |
| `omgb use` (computer use: desktop + browser MCP) | ✅ | ✅ | ✅ |
| `omgb browser` (browser use MCP) | ✅ | ✅ | ✅ |
| `omgb schedule` / `omgb cron` CLI | ✅ | ✅ | ✅ |
| Scheduler daemon with safe lifecycle | ✅ | ✅ | ✅ |
| `/loop` slash command in `omgb connect` | ✅ | ✅ | ✅ |
| `/schedule` slash command in `omgb connect` | ✅ | ✅ | ✅ |
| `/btw` side-chat command in `omgb connect` | ✅ | ✅ | ✅ |
| Gracious rate-limit messages | ✅ | ✅ | ✅ |
| Mobile app pairing via QR code | ✅ | 🚧 build/typecheck only | 🚧 |
| Mobile `/loop` | ✅ | ✅ | ✅ |
| Mobile `/schedule` | ✅ | ✅ | ✅ |
| Mobile `/btw` | ✅ | ✅ | ✅ |
| SSRF / private-IP URL filtering for browser and connect | ✅ | ✅ | ✅ |
| Desktop-control safety (`OMGB_ALLOW_DESKTOP_CONTROL`, key validation) | ✅ | ✅ | ✅ |
| `omgb serve` ACP relay with QR code, rate-limiting, origin checks | ✅ | ✅ | ✅ |
| BYOK provider management (`omgb provider`, `omgb model`) | ✅ | ✅ | ✅ |
| Local model discovery (`omgb provider discover`) | ✅ | ✅ | ✅ |
| `omgb exec`, `omgb team`, `omgb swarm`, `omgb subagent` | ✅ | ✅ | ✅ |
| `omgb loop` (iterative Grok until clean working tree) | ✅ | ✅ | ✅ |
| `omgb timeline` event logging | ✅ | ✅ | ✅ |
| `omgb harness` (connectors for OpenCode, Codex, Claude, Hermes, Pi, OMP) | ✅ | ✅ | ✅ |
| MCP tool servers (memory, browser, computer) | ✅ | ✅ | ✅ |
| Safe env filtering (`*_API_KEY` only, no `PATH`/`LD_PRELOAD`) | ✅ | ✅ | ✅ |
| `runTerminalCmd` RPC for mobile → local CLI execution | ✅ | ✅ | ✅ |

## Phase 2 — Harness gap roadmap

| Feature | Implemented | Tested | Working |
| --- | --- | --- | --- |
| Self-improving / auto skill creation from completed tasks | ⏳ | ⏳ | ⏳ |
| Personal "taste" / style learning from accepts/rejects/edits | ⏳ | ⏳ | ⏳ |
| LSP + DAP integration (semantic refactor, debugger attach) | ⏳ | ⏳ | ⏳ |
| Hashline / safe token-efficient edits with mismatch rejection | ⏳ | ⏳ | ⏳ |
| Advanced multi-agent team orchestration with isolated git worktrees | ⏳ | ⏳ | ⏳ |
| Persistent cross-session memory (SQLite/JSONL, hindsight, playbooks) | ⏳ | ⏳ | ⏳ |
| Session branching / resuming / forking | ⏳ | ⏳ | ⏳ |
| Git-native safety hooks (auto-commit per edit, review/undo/rebase helpers) | ⏳ | ⏳ | ⏳ |
| PR automation / GitHub agent | ⏳ | ⏳ | ⏳ |
| Plugin / skill / hook marketplace and hot-loading | ⏳ | ⏳ | ⏳ |
| Headless / CI mode with deterministic playbooks | ⏳ | ⏳ | ⏳ |
| Multi-model cost routing / benchmark-optimized scaffolding | ⏳ | ⏳ | ⏳ |
| Local-first inference fallback (Ollama / LM Studio / vLLM) | 🚧 provider discover exists | ⏳ | ⏳ |

## Verification commands

- `tools/oh-my-grok-build`: `npm run typecheck && npm run build && npm test && npm run format:check`
- `grok-build-app`: `npm run typecheck && npm run build && npm run test:unit && npm run format:check`
- Rust workspace: `cargo fmt --check`

## Notes

- All Phase 1 features now have passing tests and builds. Mobile QR pairing is implemented, type-safe, and buildable; runtime camera/QR testing is still pending.
- Phase 2 items are the long-term gaps relative to the harness landscape (`codex`, `opencode`, `claude code`, `omp`, `command code`, `devin`, `hermes`, `aider`, `pi`, etc.). They should be broken into testable units as they are implemented.
