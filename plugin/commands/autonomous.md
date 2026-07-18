---
name: autonomous
description: Fully autonomous mode via `omgb autonomous`. Runs with --yolo and warns if the Grok sandbox profile is not set to a non-off value.
---

# /autonomous — high-autonomy mode

1. Treat the user's last message as a mission, not a single question.
2. Run it with `omgb autonomous "<mission>" [--model ...] [--sandbox-profile <profile>]`.
3. The command always passes `--yolo` (auto-approve tool calls) to Grok.
4. Check `~/.grok/config.toml` for `[sandbox].profile` before launching. Warn on stderr if it is missing or set to `off`, because autonomous mode should only run inside a sandbox.
5. Pass `--sandbox-profile <profile>` to set the `GROK_SANDBOX_PROFILE` environment variable for the spawned Grok process (best effort).
6. Still respect hard guards:
   - Do not delete files or directories without explicit user confirmation.
   - Do not run commands that could damage the system or exfiltrate data.
   - Stop and ask if the task scope expands beyond the original request.
7. Prefer subagents for parallel research, testing, or review.
8. Commit safely via the `auto-commit` skill when a milestone is reached.
9. Summarize results and any commands the user should run manually.
