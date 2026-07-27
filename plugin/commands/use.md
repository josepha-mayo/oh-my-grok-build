---
name: use
description: Run a computer-use prompt that can control the desktop and browser via `omgb use`.
allowed-tools: run_terminal_cmd
---

# /use — computer use

1. Identify the user's desktop/browser task.
2. Run `omgb use "<prompt>" [--model <model>] [--yolo]`.
3. Desktop control is gated: pass `--yolo` **and** set `OMGB_ALLOW_DESKTOP_CONTROL=1` (the env var enables control; `--yolo` auto-approves tool calls).
4. Stream the result back. If the agent asks for permission, choose `allow_once` when reasonable.
5. Summarize what was done.
