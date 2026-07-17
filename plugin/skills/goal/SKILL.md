---
name: goal
description: Take a high-level goal, spawn a planning subagent, and track progress. Use when the user gives a multi-step objective and wants autonomous execution with status checks.
allowed-tools: spawn_subagent, run_terminal_cmd
---

# /goal skill

1. Capture the goal from the user's message.
2. Spawn a planning subagent with the goal and a request to produce a short task list.
3. Display the plan and start executing each step:
   - Spawn worker subagents for independent tasks when useful.
   - Run terminal commands for local operations.
   - Update progress after each completed step.
4. Stop if blocked for more than one turn, if a step fails, or if a safety boundary is hit.
5. End with a concise summary of what was done and what remains.
