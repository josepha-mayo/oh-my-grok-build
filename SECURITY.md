# Security Hardening Guide

`omgb` is designed to run on a developer's machine, relay between a trusted phone and local agents, and call model APIs with user-provided keys. This guide describes the controls that keep that safe.

## Threat model

- **Untrusted network**: the mobile app connects to `omgb serve` on the local LAN; the relay must not allow arbitrary code execution or exfiltration.
- **Untrusted URL / SSRF**: user-supplied or model-suggested URLs must not reach cloud metadata services or private infrastructure unless explicitly allowed.
- **Secret leakage**: API keys, connector secrets, and the WebSocket pairing secret must stay out of logs, Git, and the model context.
- **Malicious shell commands**: the `safe-shell-guard` binary blocks disallowed commands before execution.
- **Symlink / path traversal**: file-system helpers reject symlinks and paths that escape the working directory.

## Implemented controls

### Pairing and relay

- Pairing secrets are 256-bit values generated directly from the operating system's cryptographically secure random source, encoded as 64 hexadecimal characters, and compared with `blake3` + `constant_time_eq`.
- `omgb serve` applies per-IP rate limiting and only upgrades the WebSocket after the secret is accepted. A non-loopback listener requires an explicit Origin allowlist; loopback listeners enforce one when configured.
- The unauthenticated `GET /healthz` endpoint exposes only a fixed service name, package version, and `ok` status. The relay terminates if its embedded ACP agent exits, preventing a false-ready listener from lingering.
- When `--allowed-origins` is configured, the same explicit allowlist controls browser WebSocket origins and CORS for authenticated group HTTP requests. CORS is disabled when no allowlist is configured; `*` is an explicit operator choice, not a default.
- An HTTPS-hosted browser client must use a TLS-terminated `wss://` relay. The native app may use a local `ws://` relay on a trusted LAN; browsers cannot due to mixed-content protections.
- The persisted pairing secret is stored with `0600` permissions on Unix and opened with `O_NOFOLLOW`. On Windows it is opened with `FILE_FLAG_OPEN_REPARSE_POINT`; reparse-point and symlink files are rejected.
- `crates/oh-my-grok-build/src/net.rs` blocks cloud metadata hosts, private/loopback ranges when public access is expected, and DNS-rebinding attacks. Redirects are not followed and resolved IPs are pinned.

### Secret storage

- Provider API keys and connector secrets are stored in `~/.omgb/.env` with `0600` permissions and referenced by `env_key` in `~/.grok/config.toml` and `~/.omgb/connectors.json`.
- Group invite/member/agent tokens and local message archives are written atomically with owner-only permissions. Legacy message archives have the restrictive mode or ACL reapplied on their next write.
- Logs and traces redact secrets. The mobile app strips `server_key`/`server-key` from the URL before logging and uses `Sec-WebSocket-Protocol` as a fallback for the secret.
- The `safe-shell-guard` parser blocks commands that export or assign sensitive environment variables (`LD_PRELOAD`, `PATH`, `HOME`, etc.).

### Shell and tool guard

- `safe-shell-guard` is a Rust binary that classifies commands as `Allow` or `Deny` using an explicit allow/deny list. It rejects path traversal, recursive `rm`, multi-command chains, redirections, and many forms of variable/quote abuse.
- `omgb` never auto-approves dangerous tools unless `--yolo` or `always-approve` is explicitly set.

### Build and supply chain

- `.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo clippy`, and `cargo test` on every PR.
- `.github/workflows/release.yml` builds cross-platform release artifacts, generates SHA-256/SHA-512 checksums, produces SBOM artifacts, and uses GitHub build-provenance attestation.
- `Cargo.lock` and `package-lock.json` are committed so installs are reproducible.

## Hardening checklist

1. Run `omgb doctor` after installation to verify the environment.
2. Keep `~/.omgb/.env` at `0600` / owner-only and never commit it.
3. Use `omgb serve --bind 127.0.0.1` unless you explicitly want LAN access; use `--insecure-allow-lan` only on a trusted network.
4. Set up the mobile app with the generated QR code; do not share the pairing URL or secret.
5. Review `~/.omgb/connectors.json` and `~/.grok/config.toml` for stale `env_key` references.
6. Keep `omgb` and `safe-shell-guard` together by using `omgb update --apply`. For source builds, copy `target/release/safe-shell-guard` (or `.exe`) into `plugin/bin/` after each build.

## Reporting issues

Report security bugs through `omgb feedback` or by opening a private security advisory on GitHub. Do not disclose details publicly until a fix is released.
