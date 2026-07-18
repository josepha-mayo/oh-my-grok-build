---
name: memory
description: Update and read a bounded memory file at ~/.omgb/MEMORY.md. Use to store short facts at session end and load them at session start for context.
allowed-tools: read_file, run_terminal_cmd
---

# /memory skill

File: `~/.omgb/MEMORY.md`.

1. **At session start**, read `~/.omgb/MEMORY.md` with `read_file` (or `cat`) and summarize the most relevant bullets in one sentence.
2. **To remember a fact**, append a `- ` bullet to the file:
   ```sh
   node plugin/bin/memory.js add "short fact"
   ```
3. **Keep it bounded**: when the file exceeds 50 bullets, remove oldest entries:
   ```sh
   node plugin/bin/memory.js trim
   ```
4. **At session end**, append any new short facts learned and briefly note what was done.
