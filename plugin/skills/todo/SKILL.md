---
name: todo
description: Maintain a small JSON task list in ~/.omgb/todo.json. Use for create, list, complete, and delete operations.
allowed-tools: run_terminal_cmd
---

# /todo skill

Storage: `~/.omgb/todo.json` (`node -e "require('os').homedir()"` to confirm).

Create the directory if needed, then operate with `node -e` one-liners:

- **create** `add <text>`:
  ```sh
  node -e "const fs=require('fs'),p=require('path'),d=p.join(require('os').homedir(),'.omgb');fs.mkdirSync(d,{recursive:true});const f=p.join(d,'todo.json');let l=[];try{l=JSON.parse(fs.readFileSync(f,'utf8'))}catch{}l.push({id:Date.now(),text:process.argv[1],done:false});fs.writeFileSync(f,JSON.stringify(l,null,2))" "the task text"
  ```

- **list**:
  ```sh
  node -e "const fs=require('fs'),p=require('path');const f=p.join(require('os').homedir(),'.omgb','todo.json');let l=[];try{l=JSON.parse(fs.readFileSync(f,'utf8'))}catch{}console.log(JSON.stringify(l,null,2))"
  ```

- **complete** `<id>`:
  ```sh
  node -e "const fs=require('fs'),p=require('path');const f=p.join(require('os').homedir(),'.omgb','todo.json');let l=[];try{l=JSON.parse(fs.readFileSync(f,'utf8'))}catch{}const t=l.find(x=>x.id==process.argv[1]);if(t){t.done=true;fs.writeFileSync(f,JSON.stringify(l,null,2))}" <id>
  ```

- **delete** `<id>`:
  ```sh
  node -e "const fs=require('fs'),p=require('path');const f=p.join(require('os').homedir(),'.omgb','todo.json');let l=[];try{l=JSON.parse(fs.readFileSync(f,'utf8'))}catch{}l=l.filter(t=>t.id!=process.argv[1]);fs.writeFileSync(f,JSON.stringify(l,null,2))" <id>
  ```

Output the result as a concise bulleted list.
