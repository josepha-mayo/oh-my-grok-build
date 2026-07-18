---
name: todo
description: Maintain a small JSON task list in ~/.omgb/todo.json. Use for create, list, complete, and delete operations.
allowed-tools: run_terminal_cmd
---

# /todo skill

Storage: `~/.omgb/todo.json`.

Operate with the helper script:

- **create** `add <text>`:
  ```sh
  node plugin/bin/todo.js add "the task text"
  ```

- **list**:
  ```sh
  node plugin/bin/todo.js list
  ```

- **complete** `<id>`:
  ```sh
  node plugin/bin/todo.js done <id>
  ```

- **delete** `<id>`:
  ```sh
  node plugin/bin/todo.js delete <id>
  ```

Output the result as a concise bulleted list.
