---
name: research
description: Deep-research an arXiv topic and produce a report with a proposed patch using `omgb research`.
allowed-tools: run_terminal_cmd
---

# /research — deep arXiv research

1. Identify the core topic from the user's message.
2. Run `omgb research "<topic>" [--count <n>] [--model <model>]`.
3. Read the generated `~/.omgb/research/<topic>.md` report.
4. If a `~/.omgb/research/<topic>.patch` exists, read it too.
5. Summarize the findings, the identified research gaps, and the proposed patch or novel idea.
6. Suggest next steps (e.g., run `omgb loop` to implement the patch, or `omgb subagent` for parallel exploration).
