---
name: auto-commit
description: Create a safe, descriptive git commit after a batch of edits. Use when the user asks to commit or when you finish a self-contained change and the repo has uncommitted changes.
allowed-tools: run_terminal_cmd
---

# Auto-commit skill

1. Run `git status --short` to see changed files.
2. Run `git diff --staged` and `git diff` to understand changes.
3. Write a conventional-commit style message under 72 characters summarizing the change.
4. Stage with `git add -A` only if the user has not already staged; if they staged selectively, respect that.
5. Commit with `git commit -m "<type>: <summary>"`.
6. If pre-commit hooks fail, surface the error and stop; do not force.
