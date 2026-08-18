---
name: local
description: Discover every known, configured, or explicitly supplied local OpenAI-compatible model and select one as the default.
---

# /local — discover and select a local model

Run `omgb provider discover --select`. It probes all loopback endpoints in the
local-provider catalog plus configured providers and `OMGB_LOCAL_ENDPOINTS`,
prints every returned model, and asks the user to select a default. For an
unusual server or port, use `omgb provider discover --url
http://127.0.0.1:PORT/v1 --select`. Never scan non-loopback hosts implicitly.
