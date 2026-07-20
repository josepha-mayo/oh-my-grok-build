---
name: plan
description: Enter plan mode or request a step-by-step plan before executing code changes.
allowed-tools: run_terminal_cmd
---

# /plan — plan mode

1. If the user is in the `omgb` TUI, use the built-in `/plan` slash command (or Shift+Tab) to toggle plan mode for the next agent run.
2. For a one-time CLI plan, run `omgb exec "<prompt>\n\nFirst, write a concise step-by-step plan. Do not edit files until the user approves." [--model ...]`.
3. To spawn a planning-only agent, use `omgb exec --agent grok-build-plan "<task>" [--model ...]`.
4. Summarize the plan and wait for explicit user approval before making changes.
