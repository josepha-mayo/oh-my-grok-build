#!/usr/bin/env sh
set -eu

DIST_DIR=${1:?usage: generate-homebrew-formula.sh DIST_DIR vMAJOR.MINOR.PATCH}
TAG=${2:?usage: generate-homebrew-formula.sh DIST_DIR vMAJOR.MINOR.PATCH}
printf '%s\n' "$TAG" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$' || {
  echo "error: release tag must look like vMAJOR.MINOR.PATCH" >&2
  exit 1
}

checksum() {
  file="$DIST_DIR/$1"
  [ -f "$file" ] || { echo "error: missing release archive $file" >&2; exit 1; }
  sha256sum "$file" | awk '{print $1}'
}

LINUX_X64=$(checksum omgb-x86_64-unknown-linux-gnu.tar.gz)
LINUX_ARM64=$(checksum omgb-aarch64-unknown-linux-gnu.tar.gz)
MACOS_X64=$(checksum omgb-x86_64-apple-darwin.tar.gz)
MACOS_ARM64=$(checksum omgb-aarch64-apple-darwin.tar.gz)
VERSION=${TAG#v}

cat > "$DIST_DIR/omgb.rb" <<EOF
class Omgb < Formula
  desc "Productivity, orchestration, and mobile-relay harness for Grok Build"
  homepage "https://github.com/josepha-mayo/oh-my-grok-build"
  version "$VERSION"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/josepha-mayo/oh-my-grok-build/releases/download/$TAG/omgb-aarch64-apple-darwin.tar.gz"
      sha256 "$MACOS_ARM64"
    else
      url "https://github.com/josepha-mayo/oh-my-grok-build/releases/download/$TAG/omgb-x86_64-apple-darwin.tar.gz"
      sha256 "$MACOS_X64"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/josepha-mayo/oh-my-grok-build/releases/download/$TAG/omgb-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "$LINUX_ARM64"
    else
      url "https://github.com/josepha-mayo/oh-my-grok-build/releases/download/$TAG/omgb-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$LINUX_X64"
    end
  end

  def install
    target = if OS.mac?
      Hardware::CPU.arm? ? "aarch64-apple-darwin" : "x86_64-apple-darwin"
    else
      Hardware::CPU.arm? ? "aarch64-unknown-linux-gnu" : "x86_64-unknown-linux-gnu"
    end
    bin.install "omgb-#{target}" => "omgb"
    prefix.install "plugin-#{target}" => "plugin"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/omgb --version")
  end
end
EOF
