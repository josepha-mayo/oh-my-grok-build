---
name: clarify
description: When you are unsure about intent, scope, or a missing detail, ask the user one focused question and wait for an answer before continuing.
allowed-tools: ask_user
---

# /clarify skill

1. Identify exactly what is unclear or blocking progress.
2. Formulate a single, concise question with any options the user can pick from.
3. Ask the user with `ask_user` and stop all other work until they reply.
4. After the answer, confirm your understanding in one sentence, then proceed.
