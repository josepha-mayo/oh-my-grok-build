#!/usr/bin/env sh
set -eu

REPO="josepha-mayo/oh-my-grok-build"
WORKFLOW="josepha-mayo/oh-my-grok-build/.github/workflows/release.yml"
INSTALL_ROOT="${OMGB_INSTALL_ROOT:-$HOME/.local/omgb}"
VERSION="${1:-${OMGB_VERSION:-}}"

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { echo "error: tar is required" >&2; exit 1; }
command -v install >/dev/null 2>&1 || { echo "error: install is required" >&2; exit 1; }

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64) TARGET="x86_64-unknown-linux-gnu" ;;
  Linux:aarch64|Linux:arm64) TARGET="aarch64-unknown-linux-gnu" ;;
  Darwin:x86_64|Darwin:amd64) TARGET="x86_64-apple-darwin" ;;
  Darwin:arm64|Darwin:aarch64) TARGET="aarch64-apple-darwin" ;;
  *) echo "error: unsupported platform $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

if [ -z "$VERSION" ]; then
  VERSION=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")
  VERSION=${VERSION##*/}
fi
printf '%s\n' "$VERSION" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$' || {
  echo "error: release version must look like vMAJOR.MINOR.PATCH" >&2
  exit 1
}

ARCHIVE="omgb-$TARGET.tar.gz"
BASE_URL="https://github.com/$REPO/releases/download/$VERSION"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/omgb-install.XXXXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

curl -fL "$BASE_URL/$ARCHIVE" -o "$TMP_ROOT/$ARCHIVE"
curl -fL "$BASE_URL/checksums-sha256.txt" -o "$TMP_ROOT/checksums-sha256.txt"

EXPECTED=$(awk -v file="$ARCHIVE" '$2 == file || $2 == "*" file { print $1; exit }' "$TMP_ROOT/checksums-sha256.txt")
case "$EXPECTED" in
  ''|*[!0-9A-Fa-f]*) echo "error: release checksum is missing or invalid" >&2; exit 1 ;;
esac
[ "${#EXPECTED}" -eq 64 ] || { echo "error: release checksum is not SHA-256" >&2; exit 1; }

if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL=$(sha256sum "$TMP_ROOT/$ARCHIVE" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
  ACTUAL=$(shasum -a 256 "$TMP_ROOT/$ARCHIVE" | awk '{print $1}')
else
  echo "error: sha256sum or shasum is required" >&2
  exit 1
fi
[ "$(printf '%s' "$EXPECTED" | tr 'A-F' 'a-f')" = "$(printf '%s' "$ACTUAL" | tr 'A-F' 'a-f')" ] || {
  echo "error: SHA-256 mismatch for $ARCHIVE" >&2
  exit 1
}

if [ "${OMGB_INSTALL_INSECURE:-0}" = "1" ]; then
  echo "warning: skipping build-provenance verification" >&2
else
  command -v gh >/dev/null 2>&1 || {
    echo "error: GitHub CLI (gh) is required for provenance verification; set OMGB_INSTALL_INSECURE=1 only if you accept the risk" >&2
    exit 1
  }
  gh attestation verify "$TMP_ROOT/$ARCHIVE" \
    --repo "$REPO" \
    --signer-workflow "$WORKFLOW" \
    --deny-self-hosted-runners
fi

EXTRACT_ROOT="$TMP_ROOT/extract"
mkdir -p "$EXTRACT_ROOT"
tar -tzf "$TMP_ROOT/$ARCHIVE" > "$TMP_ROOT/archive-entries.txt"
if grep -Eq '(^/|(^|/)\.\.(/|$))' "$TMP_ROOT/archive-entries.txt"; then
  echo "error: release archive contains an unsafe path" >&2
  exit 1
fi
tar -xzf "$TMP_ROOT/$ARCHIVE" -C "$EXTRACT_ROOT"
if find "$EXTRACT_ROOT" -type l -print -quit | grep -q .; then
  echo "error: release archive contains a symbolic link" >&2
  exit 1
fi

BIN_SOURCE="$EXTRACT_ROOT/omgb-$TARGET"
PLUGIN_SOURCE="$EXTRACT_ROOT/plugin-$TARGET"
[ -f "$BIN_SOURCE" ] || { echo "error: release archive has no omgb binary" >&2; exit 1; }
[ -d "$PLUGIN_SOURCE" ] || { echo "error: release archive has no plugin tree" >&2; exit 1; }

mkdir -p "$INSTALL_ROOT/bin" "$INSTALL_ROOT/plugin"
install -m 755 "$BIN_SOURCE" "$INSTALL_ROOT/bin/omgb"
cp -R "$PLUGIN_SOURCE/." "$INSTALL_ROOT/plugin/"
if [ -f "$INSTALL_ROOT/plugin/bin/safe-shell-guard" ]; then
  chmod 755 "$INSTALL_ROOT/plugin/bin/safe-shell-guard"
fi

echo "Installed omgb $VERSION to $INSTALL_ROOT"
case ":$PATH:" in
  *":$INSTALL_ROOT/bin:"*) ;;
  *) echo "Add this to your shell profile: export PATH=\"$INSTALL_ROOT/bin:\$PATH\"" ;;
esac
"$INSTALL_ROOT/bin/omgb" doctor || true
