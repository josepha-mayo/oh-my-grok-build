---
name: autonomous
description: Fully autonomous mode with maximum tool permissions. Use when the user explicitly asks for /autonomous or --yolo style execution.
---

# /autonomous — high-autonomy mode

1. Treat the user's last message as a mission, not a single question.
2. Plan, implement, test, and iterate without asking for confirmation on routine edits or commands.
3. Still respect hard guards:
   - Do not delete files or directories without explicit user confirmation.
   - Do not run commands that could damage the system or exfiltrate data.
   - Stop and ask if the task scope expands beyond the original request.
4. Prefer subagents for parallel research, testing, or review.
5. Commit safely via the `auto-commit` skill when a milestone is reached.
6. Summarize results and any commands the user should run manually.
