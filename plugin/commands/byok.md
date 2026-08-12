---
name: byok
description: Configure bring-your-own-key / local endpoint settings. Use when the user wants to use a custom API key, local model, or non-xAI endpoint.
---

# /byok — bring your own key or endpoint

1. Inspect `~/.grok/config.toml` and `~/.omgb/config.json` for existing provider config.
2. Ask only for non-secret metadata such as the provider endpoint and model name. Never ask the user to paste an API key into chat, a prompt, or tool output.
3. Have the user enter the key privately in their own local terminal as the one-command `OMGB_API_KEY` environment variable, then run `omgb provider add <name> --template <provider> ...`. Do not type, echo, inspect, or repeat the secret on their behalf.
4. `omgb` validates the endpoint before persisting the key to the private `~/.omgb/.env` entry named by the provider's `env_key` (for example `OMGB_<PROVIDER>_API_KEY` for custom providers or `OPENAI_API_KEY` for built-in templates). Never write secrets to the repository or Grok config.
5. For a keyless local provider, prefer `omgb provider discover --add`; no API key or subscription sign-in is required.
6. Use the custom model with `omgb exec "<prompt>" --model omgb-<name>` or set it as the default through `omgb model switch omgb-<name>`.
