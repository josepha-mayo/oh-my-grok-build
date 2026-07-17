---
name: loop
description: Start an autonomous work loop. Use when the user says /loop or wants the agent to keep iterating on a task without per-step confirmation.
---

# /loop — autonomous iteration mode

1. Confirm the high-level goal with the user if unclear, then switch to autonomous execution.
2. Maintain a visible TODO list; update it after every step.
3. For each iteration:
   - Pick the next highest-value action.
   - Execute it (read, edit, run tests, spawn subagents as needed).
   - Report a one-line status in the scrollback.
   - Stop if the task is complete, if you are blocked for more than one turn, or if you hit a safety boundary.
4. Ask for confirmation before irreversible operations (deletes, force-pushes, large refactors).
5. End with a concise summary of what changed.
