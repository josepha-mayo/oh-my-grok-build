# Privacy and Telemetry Policy

`omgb` is **local-first and private-by-default**. The project is a developer tool that runs on your own machine, stores your keys on your own filesystem, and relays mobile traffic over a WebSocket that stays on your local network.

## What we do not collect

- **No telemetry by default**: upstream telemetry and feedback channels are disabled.
- **No API keys**: provider and connector secrets live in `~/.omgb/.env` and are never transmitted to the `omgb` project or model providers except in the API calls you explicitly choose to make.
- **No project-operated collection**: `omgb` does not send a separate copy of your codebase, conversation history, or usage data to the OMGB maintainers.
- **No analytics cookies**: the CLI and mobile app do not use advertising or analytics identifiers.

## What may be shared

- **Model API calls**: when you run a prompt, `omgb` sends the prompt, conversation context, and any file contents or tool outputs the agent includes to the model endpoint you configured (`xAI`, OpenAI-compatible, Ollama, etc.). Local providers keep that traffic local; remote-provider traffic is governed by that provider's privacy policy.
- **Mobile relay**: the mobile app sends messages and tool approvals to `omgb serve`. The default `127.0.0.1` binding is reachable only from the same computer. Connecting a separate phone requires `--insecure-allow-lan`, which sends the relay traffic across your trusted local network; use a TLS-terminating proxy when that network is not fully trusted.
- **Feedback**: `omgb feedback` opens a GitHub issue with the title, description, and environment metadata you provide. No secrets or source code are included.

## Data stored locally

- `~/.grok/config.toml` — Grok Build profile, model, and endpoint settings.
- `~/.omgb/config.json` — `omgb` provider and default model settings.
- `~/.omgb/.env` — API keys and connector secrets (permissions `0600`, never committed).
- `~/.omgb/schedule.jsonl` — background scheduled jobs.
- `~/.omgb/connectors.json` — connector registry without secrets.
- `~/.omgb/subagents.jsonl` — subagent registry.
- `~/.omgb/sessions/` and `~/.omgb/memory/` — session history and cross-session memory.
- `~/.omgb/groups/` — local group definitions and message history. Group definitions contain invite, member, and remote-agent tokens and are written with owner-only permissions.
- `~/.omgb/group_memberships.json`, `hosted_agents.json`, and `pending_joins.json` — local group membership and short-lived approval credentials, written atomically with owner-only permissions.
- `grok-build-app` credential storage — native builds store the pairing secret in the OS-backed secure store (`expo-secure-store`) with a 30-day TTL and keep saved group memberships there until **Forget** is selected. The web companion uses origin-scoped `sessionStorage` instead, so pairing and group credentials are cleared when the browser session ends.

## Your controls

- Delete `~/.omgb/.env` and `~/.omgb/` to remove all local data.
- Use **Forget** beside a saved group membership in the mobile app to remove that member token from its secure store.
- Run `omgb serve --bind 127.0.0.1` to keep the mobile relay on localhost.
- Disable memory or background scheduling in `~/.omgb/config.json` if you do not want those features.
- Use `--no-memory` or disable `memory` in config to prevent cross-session recall.

## Changes to this policy

Major changes will be released with a new version and noted in the release notes. The current policy always lives at `PRIVACY.md` in the repository root.
