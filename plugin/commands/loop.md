---
name: loop
description: Start a Devin-style autonomous work loop via `omgb devin loop`. Requires a clean git working tree and iterates until the tree is clean or the max iteration limit is reached.
---

# /loop — autonomous iteration mode

1. Confirm the high-level goal with the user if unclear.
2. Require a clean git working tree before starting; error otherwise.
3. Run the initial prompt with `omgb devin loop "<goal>" [--model ...] [--max-iterations 5]`.
4. After each Grok run:
   - Run `git diff` and `git status --short`.
   - If the working tree is dirty, send Grok a follow-up prompt asking it to review the diff and fix issues.
   - Stop when `git status --short` is empty or after `--max-iterations`.
5. Log each iteration to `~/.omgb/logs/devin-loop.jsonl`.
6. End with a concise summary of what changed.
