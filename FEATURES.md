# Oh My Grok Build — Goals, Features and Roadmap

This file is the single source of truth for what `oh-my-grok-build` is and what still needs to happen before it is production-ready.

> **Project north star:** `oh-my-grok-build` is a first-class extension of the open-source `xai-org/grok-build` **Rust harness**, not a separate tool or language rewrite. We build *on top of* the existing Rust crates (`xai-grok-pager`, `xai-grok-shell`, `xai-grok-mcp`, etc.), add missing harness features in Rust, and ship the binary as `oh-my-grok-build` (alias `omgb`). The legacy TypeScript/Node wrapper under `tools/oh-my-grok-build` has been removed. The mobile app is a separate React Native + Expo project in the `grok-build-app` repo.

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
| `crates/oh-my-grok-build` | New composition-root binary `oh-my-grok-build` / `omgb`. It imports the upstream `xai-grok-*` crates and contains all new subcommands (provider, model, exec, team, workflow, research, serve, dap, lsp, etc.). |
| `crates/oh-my-grok-build/src/providers.rs`, `moe.rs` | BYOK providers, local model discovery, model switching, and cost routing. |
| `crates/oh-my-grok-build/src/scheduler.rs` | Cron/scheduled prompt execution, background daemon, and lifecycle handling. |
| `crates/oh-my-grok-build/src/subagents.rs`, `team.rs`, `swarm.rs` | Subagent commands, task splitting, and worktree isolation. |
| `crates/oh-my-grok-build/src/taste.rs` | Personal taste/style learning from accepts, rejects, and edits. |
| `crates/oh-my-grok-build/src/timeline.rs` | Cross-session event logging and the `timeline` command. |
| `crates/oh-my-grok-build/src/research.rs` | ArXiv/web research and patch proposal. |
| `crates/oh-my-grok-build/src/harness.rs` | Cross-harness connectors for OpenCode, Codex, Claude, Hermes, Pi, OMP, and others. |
| `crates/oh-my-grok-build/src/server.rs`, `group.rs` | ACP/WebSocket mobile relay plus group APIs, pairing, rate limiting, and origin/secret checks. |
| `plugin/` | Grok Build plugin skills and slash commands (`/use`, `/browser`, `/schedule`, `/loop`, `/btw`, `/taste`, `/autonomous`, `/research`, `/workflow`, `/live`, etc.). |
| `grok-build-app/` (separate repo) | React Native + Expo mobile app. |
| `AGENTS.md` | Agent rules and conventions. |
| `FEATURES.md` | This file. |

## 3. Phase status

Legend: `✅` verified in Rust, `🚧` in progress, `⏳` planned, `N/A` out of scope.

### Phase 0 — Repo reset and plan

| Feature | Status |
| --- | --- |
| Remove `tools/oh-my-grok-build` (Node harness) out of `tools/` (archived) | ✅ |
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
| `omgb auth` — optional Grok subscription status/device login/logout, separate from BYOK | ✅ |
| `omgb provider discover` — local model discovery (Ollama/LM Studio) | ✅ |
| `omgb model` — switch default model, list custom models | ✅ |
| `omgb exec` — single-turn headless prompt | ✅ |
| `omgb loop` — iterate until working tree is clean | ✅ |
| `omgb cron` / `omgb schedule` — scheduled prompt execution | ✅ |
| `omgb team` — team mode with isolated git worktrees | ✅ |
| `omgb swarm` — parallel subagents with task splitting and majority-vote fallback | ✅ |
| `omgb subagent spawn/list/kill/logs/trace` | ✅ |
| `omgb thread` — persistent cross-session messaging with bounded inbox delivery | ✅ |
| `omgb workflow` — exec/fan_out/shell workflow runner | ✅ |
| `omgb research` — arXiv/web research and patch proposal | ✅ |
| `omgb timeline` — recent session/job events | ✅ |
| `omgb harness` — drive OpenCode, Codex, Claude, Hermes, Pi, OMP CLI agents | ✅ |
| `omgb serve` — ACP relay with QR code, secret, origin/rate checks | ✅ |
| `omgb connect <url>` — CLI ACP client | ✅ |
| `omgb use` / `omgb browser` — desktop/browser MCP control | ✅ |
| `omgb mcp` — memory, browser, computer MCP server management | ✅ |
| Web-search tool pack — Tavily, Brave, Serper, Google, Bing, SearXNG, DuckDuckGo | ✅ |
| `/workflow` and `/live` slash commands in the pager | ✅ |
| Nested subagent permission chain with depth limits | ✅ |
| Taste learning (`/taste`) injected into prompts | ✅ |
| Slash commands in connect/TUI: `/loop`, `/schedule`, `/btw`, `/plan`, `/yolo`, `/autonomous`, `/taste`, `/use`, `/browser`, `/research`, `/workflow`, `/live` | ✅ |
| SSRF/private-IP/cloud-metadata URL filtering for browser, fetch, and connect | ✅ |
| Safe env filtering for providers/MCP (`*_API_KEY` only, block `PATH`/`LD_PRELOAD`/etc.) | ✅ |
| Desktop-control safety (`OMGB_ALLOW_DESKTOP_CONTROL` gating) | ✅ |
| `omgb commit` / `omgb review` / `omgb undo` helpers | ✅ |
| Tests for every new crate (`cargo test -p oh-my-grok-build`) | ✅ |
| `cargo fmt`, `cargo clippy`, `cargo test` green on CI | 🚧 |

> Phase 1 features are implemented as modules inside `crates/oh-my-grok-build`; the separate `omgb-*` crates listed in the repo layout may be extracted once the harness stabilizes.
>
> CI status (verified 2026-08-09): the workflows contain cross-platform formatting, Clippy, test, and hook-binary checks, but the latest public runs are failing. Local Windows validation is being rerun against the current working tree. Full `cargo clippy --workspace` and `cargo test --workspace` are intentionally not run on Windows because upstream codegen crates contain Unix-only code.

### Phase 2 — Advanced harness gaps

| Feature | Status |
| --- | --- |
| Self-improving / auto skill creation from completed tasks | ✅ |
| LSP + DAP integration (semantic refactor, debugger attach) | ✅ |
| Hashline / safe token-efficient edits with mismatch rejection | ✅ |
| Persistent cross-session memory (SQLite/JSONL, hindsight, playbooks) | ✅ |
| Session branching / resuming / forking | ✅ |
| PR automation / GitHub agent | ✅ |
| Plugin / skill / hook marketplace | ✅ |
| Plugin / skill / hook marketplace hot-loading | ✅ |
| Headless / CI mode with deterministic playbooks | ✅ |
| Multi-model cost routing / benchmark-optimized scaffolding | ✅ |
| Local-first inference fallback (Ollama / LM Studio / vLLM) wired end-to-end | ✅ |
| Doctor TUI remediation | ✅ |
| Auto-mode classifier with explicit per-call approval (no inferred allow-rule persistence) | ✅ |
| Taste learning automatic from accepts/rejects/edits | ✅ |
| Anti-loop guard (16 repeated tool calls) | ✅ |

### Phase 3 — Production release

| Feature | Status |
| --- | --- |
| Signed release archives and verified Unix/Windows first-install scripts | 🚧 |
| Generated Homebrew formula with per-platform checksums | 🚧 |
| Native Debian packages for AMD64 and ARM64 | 🚧 |
| Native RPM packages for x86_64 and ARM64 | 🚧 |
| Native Windows x64 MSI | 🚧 |
| Generated WinGet manifests tied to the stable MSI checksum | 🚧 |
| GitHub Releases with signed binaries and SBOM | 🚧 |
| CI (GitHub Actions) runs `cargo test`, `cargo clippy`, `cargo fmt --check`, cross-platform builds | 🚧 |
| App-store-ready native mobile app in separate repo | 🚧 |
| User-facing docs (`README.md`, `docs/`) and man pages / `--help` | ✅ |
| Security hardening guide, telemetry policy, privacy policy | ✅ |
| Update mechanism (`omgb update`) with release channel support | ✅ |

## 4. Mobile app

- The mobile app is **not** in this harness repo. It is a separate React Native + Expo project at `github.com/josepha-mayo/grok-build-app`.
- It communicates over ACP/WebSocket with `omgb serve` on the local machine.
- It supports both `omgb` and the upstream `grok` ACP servers (same protocol, secret in pairing URL).
- It uses QR pairing, secure credential storage, chat, tool approval, model picker, slash commands, message paging, and a `/live` voice/text screen.

## 5. Verification commands

```bash
# Rust workspace
cargo fmt -p oh-my-grok-build
cargo clippy -p oh-my-grok-build --all-targets
cargo test -p oh-my-grok-build

# Distribution build
cargo build --bin omgb --profile release-dist
```

## 6. Notes

- The legacy TypeScript/Node wrapper has been removed; the `omgb` Rust binary is the production code.
- The `grok-build-app` mobile repo uses React Native/Expo and is not part of the Rust workspace.
- Upstream Rust crates in `crates/codegen/xai-grok-*` should be edited sparingly. Prefer new `omgb-*` crates and public upstream APIs. If an upstream seam is missing, open an issue/PR to expose it rather than forking logic.
- All new code follows `rustfmt.toml`/`clippy.toml`, keeps secrets out of logs, and never auto-approves dangerous tools unless `yolo`/`always-approve` is explicitly set.
