---
name: devin
description: Devin-style command group. Use `omgb devin loop` for iterative diff-driven work or `omgb devin autonomous` for fully approved sandboxed execution.
---

# /devin — Devin-style orchestration commands

## `omgb devin loop <prompt>`

- Requires a clean git working tree.
- Runs Grok with the prompt, then repeatedly checks `git diff` / `git status --short`.
- Sends follow-up prompts to review and fix the diff until the tree is clean or `--max-iterations` (default 5) is reached.
- Logs every iteration to `~/.omgb/logs/devin-loop.jsonl`.

## `omgb devin autonomous <prompt>`

- Runs the prompt with `--yolo` (always-approve).
- Reads `~/.grok/config.toml` and warns if `[sandbox].profile` is missing or `off`.
- Use `--sandbox-profile <profile>` to set `GROK_SANDBOX_PROFILE` for the child process.
