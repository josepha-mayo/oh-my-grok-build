---
name: create-workflow
description: Generate a reusable multi-agent workflow from a plain-English task, smoke-check it, and save it.allowed-tools: run_terminal_cmd
---

# /create-workflow — generate a reusable workflow

1. Capture the user's plain-English task or objective.
2. Run `omgb workflow create "<task>" [--name <name>] [--model <model>] [--yolo]`.
   - The planner will produce a workflow with `exec`, `fan_out`, and `shell` steps.
3. Smoke-check the generated workflow by loading it with `omgb workflow show <name>` or `omgb workflow run <name> --dry-run`.
4. If `--dry-run` was used, review the plan with the user before saving.
5. Saved workflows live in `~/.omgb/workflows/` and can be invoked with `/workflow <name>`.
6. For workflows that need shell commands or file edits, ensure the user approves `--yolo` and a sandbox profile is active.
