---
name: byok
description: Configure bring-your-own-key / local endpoint settings. Use when the user wants to use a custom API key, local model, or non-xAI endpoint.
---

# /byok — bring your own key or endpoint

1. Inspect `~/.grok/config.toml` and `~/.omgb/config.json` for existing provider config.
2. Ask the user for the provider endpoint, model name, and API key if not set.
3. Store the key in `~/.omgb/.env` under the provider's `env_key` (e.g. `OMGB_<PROVIDER>_API_KEY` for custom providers or `OPENAI_API_KEY` for built-in templates); never write secrets to the repository or Grok config.
4. Add the provider with `omgb provider add --id <name> --api-key <key> ...` or update `~/.grok/config.toml` with a `[model.<name>]` entry pointing at the custom base URL.
5. Use the custom model by running `omgb exec "<prompt>" --model omgb-<name>` or setting `models.default` in Grok config.
