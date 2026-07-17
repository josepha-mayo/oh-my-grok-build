---
name: review
description: Review code for correctness, security, and style. Use when the user asks for /review or /review-pr.
allowed-tools: read_file, grep, run_terminal_cmd
---

# Review skill

1. Identify the scope: current diff, a PR, or named files.
2. Read the relevant code with `read_file` and search for dependencies with `grep`.
3. Check for: correctness, edge cases, security issues, performance, style consistency.
4. Output a concise numbered list with file/line references.
5. End with a verdict: `APPROVE`, `REQUEST CHANGES`, or `COMMENT`.
