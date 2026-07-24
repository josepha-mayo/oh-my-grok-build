---
name: workflow
description: Launch a saved agent workflow, or run one from a file. Workflows fan tasks across parallel agents and report back when finished.
---

# /workflow — run a saved workflow

1. If the user names a saved workflow, run `omgb workflow run <name>`.
2. If the user provides a file path, run `omgb workflow run --file <path>`.
3. Workflows are stored in `~/.omgb/workflows/` (JSON or TOML).
4. Support `exec`, `fan_out`, and `shell` step types.
5. After a successful run, summarize results and any committed changes.
