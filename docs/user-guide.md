# Oh My Grok Build User Guide

`omgb` is a Rust CLI that adds a mobile relay, subagent orchestration, local-first model fallback, and team workflows on top of Grok Build's open-source Rust core.

## Installation

### From source

```bash
git clone https://github.com/josepha-mayo/oh-my-grok-build.git
cd oh-my-grok-build
cargo build -p oh-my-grok-build --release
```

The `omgb` binary is `target/release/omgb` (or `target\release\omgb.exe` on Windows). Copy `target/release/safe-shell-guard` into `plugin/bin/` so plugin hooks resolve it.

### From a release

Download the appropriate tarball from the [GitHub Releases](https://github.com/josepha-mayo/oh-my-grok-build/releases) page. Each release includes SHA-256/SHA-512 checksums, an SBOM, and build-provenance attestation.

Current release artifacts support Linux x86_64 and ARM64, macOS Intel and Apple Silicon, and Windows x86_64. `omgb update --apply` selects the matching artifact automatically and refuses unsupported platforms.

Each release includes `install.sh` and `install.ps1`. Download the installer
from the same tagged release, inspect it, and run it with that tag:

```bash
sh install.sh vMAJOR.MINOR.PATCH
```

```powershell
.\install.ps1 -Version vMAJOR.MINOR.PATCH
```

The scripts select the correct archive, verify its SHA-256 checksum and GitHub
build provenance, reject unsafe archive paths and symbolic links, install the
binary and adjacent plugin tree, and run `omgb doctor`. GitHub CLI (`gh`) is
required for provenance verification. The explicit `OMGB_INSTALL_INSECURE=1`
or `-Insecure` escape hatch is available for offline recovery but is not
recommended. Unix installs default to `~/.local/omgb`; Windows installs default
to `%LOCALAPPDATA%\omgb` and adds its `bin` directory to the user `PATH`.

Linuxbrew and Homebrew users can download the generated formula from the same
tagged release and install it locally. The formula embeds the exact checksum
for each supported Linux/macOS architecture and installs the matching plugin
tree beside the package prefix:

```bash
curl -fLO "https://github.com/josepha-mayo/oh-my-grok-build/releases/download/vMAJOR.MINOR.PATCH/omgb.rb"
brew install --formula ./omgb.rb
```

For a first Unix install, verify and extract the archive into a directory where
the binary and plugin tree remain adjacent. Substitute the release version and
target for your platform:

```bash
VERSION=vMAJOR.MINOR.PATCH
TARGET=x86_64-unknown-linux-gnu
curl -fLO "https://github.com/josepha-mayo/oh-my-grok-build/releases/download/${VERSION}/omgb-${TARGET}.tar.gz"
curl -fLO "https://github.com/josepha-mayo/oh-my-grok-build/releases/download/${VERSION}/checksums-sha256.txt"
grep "omgb-${TARGET}.tar.gz$" checksums-sha256.txt | sha256sum -c -
gh attestation verify "omgb-${TARGET}.tar.gz" \
  --repo josepha-mayo/oh-my-grok-build \
  --signer-workflow josepha-mayo/oh-my-grok-build/.github/workflows/release.yml \
  --deny-self-hosted-runners
tar -xzf "omgb-${TARGET}.tar.gz"
INSTALL_ROOT="$HOME/.local/omgb"
mkdir -p "$INSTALL_ROOT/bin"
install -m 755 "omgb-${TARGET}" "$INSTALL_ROOT/bin/omgb"
mv "plugin-${TARGET}" "$INSTALL_ROOT/plugin"
export PATH="$INSTALL_ROOT/bin:$PATH"
omgb doctor
```

Add the `PATH` export to your shell profile after confirming `omgb doctor`
succeeds. On Windows, extract the matching `.tar.gz` with `tar -xzf`, copy
`omgb-<target>.exe` to an `omgb\\bin` directory on `PATH`, and place
`plugin-<target>` beside that `bin` directory as `plugin`. After the first
install, prefer `omgb update --apply`; it verifies provenance and overlays
managed plugin files without deleting user additions.

## Updating

```bash
# Check for a new stable release
omgb update --check

# Install the latest stable release
omgb update --apply

# Install the latest pre-release
omgb update --apply --channel nightly
```

By default `omgb update` verifies the GitHub build-provenance attestation for the downloaded archive. This requires the GitHub CLI (`gh`) to be installed. If `gh` is unavailable, you can skip verification with `--insecure` (not recommended).

On Windows, the running executable cannot be overwritten in place. `omgb`
therefore stages the verified binary and starts a hidden replacement helper that
atomically swaps it in after the current command exits. If the helper cannot be
started, the staged path and manual fallback are printed explicitly.

## Quick start

```bash
# Verify the environment
omgb doctor

# Run the TUI
omgb

# One-shot headless prompt
omgb exec "refactor src/main.rs to use Result"

# Autonomous loop with auto-approve
omgb loop "add tests for the provider module" --yolo
```

## Pairing the mobile app

```bash
omgb serve
```

The terminal prints a WebSocket URL and a QR code. The QR includes the relay URL, pairing secret, and the harness's validated absolute working directory so Windows and Unix clients open the correct project. Open the `grok-build-app` mobile app and scan it; when pairing manually or with an older QR, enter an absolute working directory on the server. The relay stays on your local network by default.

`GET /healthz` returns a small secret-free liveness document for service
managers and local diagnostics. The relay supervises its embedded ACP agent and
exits if that agent stops instead of continuing to accept connections that it
cannot serve.

`GET /capabilities` returns the relay API version, supported ACP/group/voice
features, and group message limits without exposing credentials. Current mobile
clients verify this contract before storing an OMGB pairing profile. A 404 keeps
the documented upstream `grok serve` ACP-only path available, but a malformed or
incompatible advertised OMGB contract fails with one compatibility error rather
than later protocol failures.

For LAN access:

```bash
omgb serve --bind 0.0.0.0 --insecure-allow-lan --allowed-origins '*'
```

LAN mode is intentionally opt-in and requires an origin policy. Replace `*`
with the exact browser origins you intend to permit when a browser is a client;
the native mobile app relies on the pairing secret and does not send an Origin
header. The same allowlist controls CORS for authenticated group API requests.
An HTTPS-hosted web companion requires `wss://`; use the native mobile app for
a local `ws://` relay. Prefer `wss://` behind a TLS-terminating proxy whenever
possible.

## Core commands

| Command | Purpose |
| --- | --- |
| `omgb` or `omgb tui` | Interactive TUI |
| `omgb exec "PROMPT"` | One prompt and exit |
| `omgb loop "PROMPT"` | Autonomous diff-driven loop |
| `omgb provider list` / `add` / `remove` / `cost` | BYOK/local providers and user-owned routing costs |
| `omgb auth status` / `login` / `logout` | Optional Grok subscription session (device code by default) |
| `omgb model` | List or switch active models |
| `omgb cron add "0 9 * * *" "summarize issues"` | Schedule recurring prompts |
| `omgb team` | Isolated git worktrees for team members |
| `omgb swarm` | Parallel subagent execution |
| `omgb subagent` | Spawn, list, kill, and inspect subagents |
| `omgb thread` | Persistent sessions with bounded, acknowledged peer-message delivery |
| `omgb research "TOPIC"` | Deep arXiv / web research |
| `omgb serve` | ACP relay for the mobile app |
| `omgb update` | Check for and install self-updates |
| `omgb group` | Multi-agent group chat |
| `omgb workflow` | Reusable agent workflows |
| `omgb doctor` | Environment diagnostics |

Enter provider API keys only in your own local terminal (for example through the one-command `OMGB_API_KEY` environment variable used by `omgb provider add`). Never paste a model-provider key into chat, a prompt, or a group message. Keyless local endpoints can be added with `omgb provider discover --add`, and Grok subscription sign-in remains optional.

The automatic cheapest-provider router uses built-in prices only as fallback
estimates. Pricing and selected models change, so set the value you actually
want used with `omgb provider cost ID USD_PER_MILLION_TOKENS` (or
`provider add --cost-per-million VALUE`). Inspect it with `provider cost ID` and
remove an override with `provider cost ID --reset`.

## Slash commands in the TUI

The TUI and mobile app support these slash commands:

- `/autonomous`, `/browser`, `/btw`, `/byok`, `/create-workflow`, `/dream`, `/group`, `/live`, `/loop`, `/plan`, `/recap`, `/research`, `/schedule`, `/taste`, `/use`, `/voice`, `/workflow`, `/yolo`

## Configuration

- `~/.grok/config.toml` — Grok Build profile, model, endpoints.
- `~/.omgb/config.json` — `omgb` provider and default model settings.
- `~/.omgb/.env` — API keys and connector secrets (set permissions to `0600` and never commit).

## Security

See [`SECURITY.md`](../SECURITY.md) for the threat model, implemented controls, and a hardening checklist.

## Privacy

See [`PRIVACY.md`](../PRIVACY.md) for the telemetry and data-storage policy.

## Further reading

- `README.md` — project overview and feature list
- `FEATURES.md` — roadmap and completion status
- `SECURITY.md` — security hardening guide
- `PRIVACY.md` — privacy and telemetry policy
- `plugin/commands/*.md` — slash-command documentation
