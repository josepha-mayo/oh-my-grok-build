---
name: yolo
description: Toggle or enable always-approve (YOLO) mode for the current or next omgb command.
allowed-tools: run_terminal_cmd
---

# /yolo — always-approve mode

1. In the `omgb` TUI, use the built-in `/always-approve` slash command to toggle auto-approval mode.
2. For a one-shot CLI command, append `--yolo` to `omgb exec`, `omgb use`, `omgb browser`, `omgb loop`, or `omgb autonomous`.
3. Only enable `--yolo` if the user explicitly asks for it and a sandbox profile is active in `~/.grok/config.toml`.
4. Confirm the current permission mode if the user asks without a command.
