---
name: memory
description: Update and read a bounded memory file at ~/.omgb/MEMORY.md. Use to store short facts at session end and load them at session start for context.
allowed-tools: read_file, run_terminal_cmd
---

# /memory skill

File: `~/.omgb/MEMORY.md` (use `node -e "require('os').homedir()"` to locate).

1. **At session start**, read `~/.omgb/MEMORY.md` with `read_file` (or `cat`) and summarize the most relevant bullets in one sentence.
2. **To remember a fact**, append a `- ` bullet to the file:
   ```sh
   node -e "const fs=require('fs'),p=require('path'),d=p.join(require('os').homedir(),'.omgb');fs.mkdirSync(d,{recursive:true});const f=p.join(d,'MEMORY.md');if(!fs.existsSync(f))fs.writeFileSync(f','# Agent Memory\\n\\n');fs.appendFileSync(f,'- '+process.argv[1]+'\\n')" "short fact"
   ```
3. **Keep it bounded**: if the file exceeds 50 bullets, remove oldest entries by rewriting the file. Use `node -e` to split, slice, and rewrite.
4. **At session end**, append any new short facts learned and briefly note what was done.
