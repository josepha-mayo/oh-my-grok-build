---
name: byok
description: Configure bring-your-own-key / local endpoint settings. Use when the user wants to use a custom API key, local model, or non-xAI endpoint.
---

# /byok — bring your own key or endpoint

1. Do not open or edit provider files. Use `omgb provider catalog`, `omgb provider list`, and the commands below.
2. Ask only for non-secret metadata such as provider ID, endpoint, backend, and model. Never ask the user to paste a key into chat, a prompt, or tool output.
3. Have the user set the key privately in their terminal, then configure everything by command:
   - PowerShell: `$env:OMGB_API_KEY = Read-Host -MaskInput "API key"`
   - Custom endpoint: `omgb provider add NAME --base-url URL --model MODEL --backend chat-completions --default`
   - Built-in: `omgb provider add PROVIDER --default`
   - Clear transient input: `Remove-Item Env:OMGB_API_KEY`
4. `omgb` validates the endpoint before persisting the key to the private `~/.omgb/.env` entry named by the provider's `env_key` (for example `OMGB_<PROVIDER>_API_KEY` for custom providers or `OPENAI_API_KEY` for built-in templates). Never write secrets to the repository or Grok config.
5. For a keyless local provider, prefer `omgb provider discover --select`; no API key or subscription sign-in is required.
6. Use the custom model with `omgb exec "<prompt>" --model omgb-<name>` or set it as the default through `omgb model switch omgb-<name>`.
