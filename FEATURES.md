# Oh My Grok Build — Goals, Features and Roadmap

This file is the single source of truth for what `oh-my-grok-build` is and what still needs to happen before it is production-ready.

> **Project north star:** `oh-my-grok-build` is a first-class extension of the open-source `xai-org/grok-build` **Rust harness**, not a separate tool or language rewrite. We build *on top of* the existing Rust crates (`xai-grok-pager`, `xai-grok-shell`, `xai-grok-mcp`, etc.), add missing harness features in Rust, and ship the binary as `oh-my-grok-build` (alias `omgb`). The legacy TypeScript/Node wrapper under `tools/oh-my-grok-build` and the Capacitor web mobile app under `grok-build-app` are no longer part of this repo.

## 1. Core principles

1. **Harness, not engine.** The Rust core (`crates/`) is the harness. We extend it, we do not reimplement it in TypeScript/Bun/Python.
2. **Privacy-first / local-first.** No telemetry or phoning home unless the user explicitly opts in. Relay traffic stays between the phone and the local machine.
3. **Grok-native.** Use Grok's own plugin, skill, hook, ACP, MCP, and subagent extension points.
4. **Lightweight sync.** Keep the fork close to upstream so rebasing is easy. Changes to upstream crates are minimal and upstream-friendly.
5. **Ship-quality.** Every feature has tests, type safety, docs, release artifacts, and a signed/verifiable package path.

## 2. Repository structure

| Path | Purpose |
|------|---------|
| `crates/codegen/xai-grok-*` | Upstream Grok Build Rust source. Edits kept minimal and clearly marked (`omgb:` comments or new extension crates). |
| `crates/oh-my-grok-build` | New composition-root binary `oh-my-grok-build` / `omgb`. It imports `xai-grok-pager` plus the `omgb-*` crates and registers the extra subcommands/slash commands. |
| `crates/omgb-providers` | BYOK providers, local model discovery, model-switching, provider connectivity tests. |
| `crates/omgb-scheduler` | Cron/scheduled prompt execution, background daemon, safe lifecycle. |
| `crates/omgb-subagents` | `team`, `swarm`, `subagent spawn/list/kill/logs/trace`, worktree isolation. |
| `crates/omgb-taste` | Personal taste/style learning from accepts/rejects/edits. |
| `crates/omgb-timeline` | Cross-session event logging and `timeline` command. |
| `crates/omgb-research` | ArXiv/web research and patch proposal. |
| `crates/omgb-harness` | Cross-harness connectors for OpenCode, Codex, Claude, Hermes, Pi, OMP, etc. |
| `crates/omgb-mobile-relay` | ACP/WebSocket server for the mobile app, QR pairing, rate limiting, origin/secret checks. |
| `plugin/` | Grok Build plugin skills and slash commands (`/use`, `/browser`, `/schedule`, `/loop`, `/btw`, `/taste`, `/autonomous`, `/research`, etc.). |
| `omgb-mobile/` (or separate repo) | Real native mobile app (Swift/Kotlin or Rust/Tauri). Not in this harness repo. |
| `AGENTS.md` | Agent rules and conventions. |
| `FEATURES.md` | This file. |

## 3. Phase status

Legend: `✅` verified in Rust, `🚧` in progress, `⏳` planned, `N/A` out of scope.

### Phase 0 — Repo reset and plan

| Feature | Status |
| --- | --- |
| Remove `grok-build-app` (Capacitor web app) from harness repo | ✅ |
| Move `tools/oh-my-grok-build` (Node harness) out of `tools/` (archived) | ✅ |
| Write `FEATURES.md` as the single source of truth | ✅ |
| Update `AGENTS.md` to describe Rust-first architecture | ✅ |
| Update CI to run `cargo` checks | ✅ |
| Review Rust core crates and identify extension seams | ✅ |
| Choose binary crate layout (`crates/oh-my-grok-build` composition root) | ✅ |

### Phase 1 — Core harness in Rust

| Feature | Status |
| --- | --- |
| `oh-my-grok-build` / `omgb` binary boots and calls into upstream `xai-grok-pager` | ✅ |
| `omgb provider` — add BYOK providers (OpenAI, Anthropic, xAI, OpenRouter, Ollama, LM Studio, vLLM, llama.cpp, Tabby) | ✅ |
| `omgb provider discover` — local model discovery (Ollama/LM Studio) | ✅ |
| `omgb model` — switch default model, list custom models | ✅ |
| `omgb exec` — single-turn headless prompt | ✅ |
| `omgb loop` — iterate until working tree is clean | ✅ |
| `omgb cron` / `omgb schedule` — scheduled prompt execution | ✅ |
| `omgb team` — team mode with isolated git worktrees | ✅ |
| `omgb swarm` — parallel subagents | ✅ |
| `omgb subagent spawn/list/kill/logs/trace` | ✅ |
| `omgb research` — arXiv/web research and patch proposal | ✅ |
| `omgb timeline` — recent session/job events | ✅ |
| `omgb harness` — drive OpenCode, Codex, Claude, Hermes, Pi, OMP CLI agents | ✅ |
| `omgb serve` — ACP relay with QR code, secret, origin/rate checks | ✅ |
| `omgb connect <url>` — CLI ACP client | ✅ |
| `omgb use` / `omgb browser` — desktop/browser MCP control | ✅ |
| `omgb mcp` — memory, browser, computer MCP server management | ✅ |
| Taste learning (`/taste`) injected into prompts | ✅ |
| Slash commands in connect/TUI: `/loop`, `/schedule`, `/btw`, `/plan`, `/yolo`, `/autonomous`, `/taste`, `/use`, `/browser`, `/research` | ✅ |
| SSRF/private-IP/cloud-metadata URL filtering for browser, fetch, and connect | ✅ |
| Safe env filtering for providers/MCP (`*_API_KEY` only, block `PATH`/`LD_PRELOAD`/etc.) | ✅ |
| Desktop-control safety (`OMGB_ALLOW_DESKTOP_CONTROL` gating) | ✅ |
| `omgb commit` / `omgb review` / `omgb undo` helpers | ✅ |
| Tests for every new crate (`cargo test -p oh-my-grok-build`) | ✅ |
| `cargo fmt`, `cargo clippy`, `cargo test` green on CI | ✅ |

> Phase 1 features are implemented as modules inside `crates/oh-my-grok-build`; the separate `omgb-*` crates listed in the repo layout may be extracted once the harness stabilizes.
>
> Build/CI update (2026-07-21): GitHub Actions is green on `ubuntu-latest`, `macos-latest`, and `windows-latest` for `cargo fmt --check`, `cargo clippy --workspace --all-targets` (Unix) / `cargo clippy -p oh-my-grok-build --all-targets` (Windows), `cargo test -p oh-my-grok-build`, and `cargo build -p oh-my-grok-build --bin safe-shell-guard` with the binary copied to `plugin/bin/safe-shell-guard` for hook verification. Full `cargo clippy --workspace` and `cargo test --workspace` are intentionally not run on Windows because upstream codegen crates contain Unix-only code.

### Phase 2 — Advanced harness gaps

| Feature | Status |
| --- | --- |
| Self-improving / auto skill creation from completed tasks | ⏳ |
| LSP + DAP integration (semantic refactor, debugger attach) | ⏳ |
| Hashline / safe token-efficient edits with mismatch rejection | ⏳ |
| Persistent cross-session memory (SQLite/JSONL, hindsight, playbooks) | ⏳ |
| Session branching / resuming / forking | ⏳ |
| PR automation / GitHub agent | ⏳ |
| Plugin / skill / hook marketplace and hot-loading | ⏳ |
| Headless / CI mode with deterministic playbooks | ⏳ |
| Multi-model cost routing / benchmark-optimized scaffolding | ⏳ |
| Local-first inference fallback (Ollama / LM Studio / vLLM) wired end-to-end | ⏳ |

### Phase 3 — Production release

| Feature | Status |
| --- | --- |
| Installation packages (Homebrew, cargo-binstall, MSI, DEB/RPM, signed tarball) | ⏳ |
| GitHub Releases with signed binaries and SBOM | ⏳ |
| CI (GitHub Actions) runs `cargo test`, `cargo clippy`, `cargo fmt --check`, cross-platform builds | ✅ |
| App-store-ready native mobile app in separate repo | ⏳ |
| User-facing docs (`README.md`, `docs/`) and man pages / `--help` | ⏳ |
| Security hardening guide, telemetry policy, privacy policy | ⏳ |
| Update mechanism (`omgb update`) with release channel support | ⏳ |

## 4. Mobile app

- The mobile app is **not** a web/Capacitor app in this repo. It is a separate, real native mobile project.
- It communicates over ACP/WebSocket with `omgb serve` on the local machine.
- It supports both `omgb` and the upstream `grok` ACP servers (same protocol, secret in pairing URL).
- It uses QR pairing, local notifications, voice input, message paging, model picker, slash commands, and tool output rendering.
- Repository: `github.com/josepha-mayo/grok-build-app` or `github.com/josepha-mayo/omgb-mobile`.

## 5. Verification commands

```bash
# Rust workspace
cargo fmt -p oh-my-grok-build
cargo clippy -p oh-my-grok-build --all-targets
cargo test -p oh-my-grok-build

# Distribution build
cargo build --bin oh-my-grok-build --profile release-dist
```

## 6. Notes

- The legacy TypeScript/Node wrapper has been removed; all production code is now Rust.
- Upstream Rust crates in `crates/codegen/xai-grok-*` should be edited sparingly. Prefer new `omgb-*` crates and public upstream APIs. If an upstream seam is missing, open an issue/PR to expose it rather than forking logic.
- All new code follows `rustfmt.toml`/`clippy.toml`, keeps secrets out of logs, and never auto-approves dangerous tools unless `yolo`/`always-approve` is explicitly set.
