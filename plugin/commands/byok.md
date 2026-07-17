---
name: byok
description: Configure bring-your-own-key / local endpoint settings. Use when the user wants to use a custom API key, local model, or non-xAI endpoint.
---

# /byok — bring your own key or endpoint

1. Inspect `~/.grok/config.toml` and `~/.omgb/state.json` for existing provider config.
2. Ask the user for the provider endpoint, model name, and API key if not set.
3. Store the key in environment variables or secure OS keychain; never write secrets to the repository.
4. Update `~/.grok/config.toml` with a `[models.<name>]` entry pointing at the custom base URL.
5. Use the custom model by running `omgb exec "<prompt>" --model <name>` or setting it in Grok config.
