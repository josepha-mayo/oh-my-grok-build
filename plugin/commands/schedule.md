---
name: schedule
description: Manage scheduled background jobs with `omgb cron` and `omgb schedule`.
allowed-tools: run_terminal_cmd
---

# /schedule — scheduled background jobs

1. Determine the user's intent:
   - To add a cron job: run `omgb cron --yolo "<cron-expression>" "<prompt>" [--model <model>]`.
   - To list jobs: run `omgb schedule list`.
   - To run a job now: run `omgb schedule run <name>`.
   - To delete a job: run `omgb schedule delete <name>`.
   - To start the persistent scheduler daemon: run `omgb schedule start`.
   - To stop the persistent scheduler daemon: run `omgb schedule stop`.
2. Confirm the action with the user if unclear.
3. Report the result.
