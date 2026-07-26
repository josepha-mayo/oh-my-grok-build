# Oh My Grok Build — Project Brief

_Last updated: 2026-07-26_

This repo (`oh-my-grok-build`) is an opinionated productivity, orchestration,
and mobile-relay layer on top of the open-source `xai-org/grok-build` Rust core.
It ships the `omgb` CLI and a Grok Build plugin with extra commands, plus a
companion React Native + Expo mobile app (`grok-build-app`) that pairs with
`omgb serve` over ACP/WebSocket.

## Current work in progress

1. **Production-grade verification of the existing meta/planning diff**
   - Subtasks now carry a `result` summary and are verified after each thread
     completes.
   - `threads::create` returns the resolved model.
   - `playbook` `assert_file` paths are resolved relative to the playbook CWD
     and hardened against directory traversal.
   - `omgb serve` accepts the `server_key` query parameter for browser/mobile
     connections.
   - `swarm::exec_plain` supports a restricted `--tools` list.
   - All of the above compiles, passes `cargo clippy`, and passes the existing
     `cargo test -p oh-my-grok-build` suite.

2. **New slash commands / CLI features**
   - `/voice`, `/recap`, and `/dream` are already provided by upstream
     `xai-grok-pager` / `xai-grok-shell`; they are advertised in the TUI
     `after_help`.
   - `/create-workflow` → `omgb workflow create "<task>"`.
     Generates a full JSON workflow (`exec` / `fan_out` / `shell`) from a
     plain-English task, validates it, and saves it to `~/.omgb/workflows/`.
   - `/group` → `omgb group ...`.
     - `new` creates a persisted group with 2–20 named/role-based agents,
       a shared model, and an invite token.
     - `chat` hosts the REPL; agents reply only when addressed (`@name`),
       when the topic matches their role, or when they have a relevant update.
     - `send` lets other human participants post into the same group file store.
     - `invite` prints the `omgb://group/<id>?token=...` link.
     - `@mentions` route a single direct-reply round between agents.
   - Plugin markdown docs live in `plugin/commands/create-workflow.md` and
     `plugin/commands/group.md`.

3. **Mobile app (`grok-build-app`) work pending / in scope**
   - The mobile app is a separate React Native + Expo (TypeScript) project.
   - It already pairs with `omgb serve` via ACP/WebSocket for chat, tool
     approval, model switching, slash commands, and a `/live` voice/text screen.
   - Need to add UI for:
     - `/group` — list, create, and join group chats; send messages; see agent
       replies; handle `@mentions`; share invite links.
     - `/create-workflow` — form to describe a task, call `omgb workflow create`,
       preview the generated workflow JSON, and save/run it.
     - `/voice`, `/recap`, `/dream` — rely on existing upstream flows but ensure
       they are advertised and reachable from the mobile chat screen.
   - Maintain iOS and Android compatibility; use TypeScript for all new code.
   - If the mobile app repo is not present locally, clone
     `https://github.com/josepha-mayo/grok-build-app` and edit it in place.

## Commands to run before committing

```bash
cargo fmt -p oh-my-grok-build
cargo clippy -p oh-my-grok-build --all-targets
cargo test -p oh-my-grok-build
```

For the mobile app:

```bash
npm run lint
npm test -- --watchAll=false
npx expo-doctor
npm audit
```

## Files touched in this session

- `crates/oh-my-grok-build/src/lib.rs`
- `crates/oh-my-grok-build/src/args.rs`
- `crates/oh-my-grok-build/src/group.rs` (new)
- `crates/oh-my-grok-build/src/workflow.rs`
- `crates/oh-my-grok-build/src/meta.rs`
- `crates/oh-my-grok-build/src/playbook.rs`
- `crates/oh-my-grok-build/src/server.rs`
- `crates/oh-my-grok-build/src/swarm.rs`
- `crates/oh-my-grok-build/src/threads.rs`
- `plugin/commands/create-workflow.md` (new)
- `plugin/commands/group.md` (new)
- `oh_my_grok_build_brief.md` (this file)

## Important constraints

- Do NOT place new configuration in `.claude/`, `.cursor/`, etc. Use `.devin/`
  or `~/.config/devin/` for new global config.
- Keep code compact, avoid unnecessary comments, and follow existing Rust
  conventions.
- Never log or commit secrets.
- `~/.omgb/.env` stores API keys (0600 on Unix).
- Do not update git config.
- **Push changes as they are made.** The Rust `oh-my-grok-build` repo and the
  separate `grok-build-app` repo each have their own `.git` history; commit and
  push both independently. For `oh-my-grok-build`, `grok-build-app/` is ignored
  as a nested checkout.
