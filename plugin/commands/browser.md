---
name: browser
description: Run a browser-use prompt via `omgb browser`.
allowed-tools: run_terminal_cmd
---

# /browser — browser use

1. Identify the user's browser automation task.
2. Run `omgb browser "<prompt>" [--model <model>] [--yolo] [--url <url> --allow-local|--allow-private]`.
3. Desktop/browser control is gated: pass `--yolo` **and** set `OMGB_ALLOW_DESKTOP_CONTROL=1` (the env var enables control; `--yolo` auto-approves tool calls).
4. If a non-public starting URL is needed, pass `--url <url>` and add `--allow-local` for loopback/localhost or `--allow-private` for LAN addresses.
5. Stream the result back. If the agent asks for permission, choose `allow_once` when reasonable.
6. Summarize what was done.
