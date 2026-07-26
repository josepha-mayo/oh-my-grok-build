---
name: group
description: Start a multi-agent group chat where humans and AI agents share one thread. Agents reply only when addressed or concerned, and can @mention each other.allowed-tools: run_terminal_cmd
---

# /group — multi-agent group chat

1. Ask the user how many agents (2–20), what model to use, and what names/roles each agent should have.
2. Create the group with `omgb group new "<name>" --count <n> --model <model> --names <n1,n2,...> --roles <r1,r2,...>`.
   - If the user already has a thread they want to turn into a group, create a new group with agents suited to the current conversation and summarize the context.
3. Start the host chat with `omgb group chat <id> [--human-name <name>]`.
4. Other humans can post messages with `omgb group send <id> "<message>" --human-name <name>`.
5. Agents reply only when directly addressed (`@name`), when the topic matches their role, or when they have a relevant update. They can `@mention` other agents for direct follow-ups.
6. Print the invite link with `omgb group invite <id>` and share it so other `omgb` users or mobile clients can join.
7. Keep messages concise; avoid reply loops. `/quit` or `/exit` leaves the chat.
