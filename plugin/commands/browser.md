---
name: browser
description: Run a browser-use prompt via `omgb browser`.
allowed-tools: run_terminal_cmd
---

# /browser — browser use

1. Identify the user's browser automation task.
2. Run `omgb browser "<prompt>" [--model <model>] [--yolo]`.
3. Desktop/browser control is gated: pass `--yolo` or set `OMGB_ALLOW_DESKTOP_CONTROL=1`.
4. Stream the result back. If the agent asks for permission, choose `allow_once` when reasonable.
5. Summarize what was done.
