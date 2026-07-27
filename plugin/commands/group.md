---
name: group
description: Start a multi-agent group chat where humans and AI agents share one thread. Agents reply only when addressed or concerned, and can @mention each other.
allowed-tools: run_terminal_cmd
---

# /group — multi-agent group chat

1. Ask the user how many agents (2–20), what model to use, and what names/roles each agent should have.
2. Create the group with `omgb group new "<name>" --count <n> --model <provider-id> --names <n1,n2,...> --roles <r1,r2,...> [--human-name <host-name>]`. Add `--yolo` to auto-approve tool use.
   - `<provider-id>` should be a configured provider id (e.g. `xai`, `openai`, `anthropic`) or a known model name like `grok-4.5`, `gpt-4o`, `claude-sonnet`.
   - `--names` and `--roles` are repeatable and may be comma-separated.
   - If the user already has a thread they want to turn into a group, create a new group with agents suited to the current conversation and summarize the context.
3. Start the host chat with `omgb group chat <id> --name <host-name>`. The host membership is saved automatically; pass `--token <member-token>` only if the saved membership is missing.
4. Other humans can post messages with `omgb group send <id> "<message>" --name <name>`. A saved membership is used automatically if one exists; otherwise pass `--token <member-token>`.
5. Agents reply only when directly addressed (`@name`), when the topic matches their role, or when they have a relevant update. They can `@mention` other agents for direct follow-ups.
6. Print the invite link with `omgb group invite <id>`. If `OMGB_REMOTE` is set, share the `http://...` URL; otherwise share the `omgb://` link and run `omgb serve` for remote clients.
7. Remote clients (including the mobile app) can create and join groups via the HTTP endpoints served by `omgb serve`: `POST /group` with the server secret creates a group; `POST /group/{id}/join` with `x-group-token: <invite-token>` requests membership; `GET/POST /group/{id}/messages` requires `x-member-token: <member-token>`.
   - From the CLI, request to join with `omgb group join <id> --token <invite-token> --name <your-name> [--remote <url>]`.
   - If the join is pending, poll for approval with `omgb group join-status <id> <request-id>`. If the pending request was not saved locally, pass `--remote <url>` and `--pre-auth <token>` together.
8. Add a remote agent hosted on another machine:
   - On the group machine: `omgb group remote-agent-add <id> <name> --url <callback-url> [--token <agent-token>]`. If no token is supplied, retrieve it later with `omgb group remote-agent-token <id> <name>`.
   - On the remote host: `omgb group host-agent <id> <name> [--token <agent-token>]`. If no token is supplied, retrieve it with `omgb group hosted-agent-token <id> <name>`.
9. Keep messages concise; avoid reply loops. `/quit` or `/exit` leaves the chat.
