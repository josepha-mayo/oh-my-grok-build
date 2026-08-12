---
name: live
description: Start a live voice/text session. In the terminal this activates voice dictation; in the mobile app it opens the live capture screen.
---

# /live — live voice/text session

1. Confirm the user wants to start a live session.
2. In the terminal, activate voice dictation (`/voice` or `/live`) so the user can speak naturally.
3. Transcribe audio into the active prompt and submit it as a user turn.
4. In the mobile app, open the live screen. Hold the talk button to stream PCM audio to the paired harness's authenticated `/voice` relay; it returns interim/final transcripts, and the app submits each final transcript to its active ACP session for streamed responses.
5. End the session when the user says "stop" or presses Esc/Enter on the terminal.
