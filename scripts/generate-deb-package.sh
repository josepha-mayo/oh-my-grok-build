#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 DIST_DIR TARGET VERSION" >&2
  exit 2
fi

dist_dir=$1
target=$2
version=$3

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid semantic version: $version" >&2
  exit 2
fi

case "$target" in
  x86_64-unknown-linux-gnu) deb_arch=amd64 ;;
  aarch64-unknown-linux-gnu) deb_arch=arm64 ;;
  *)
    echo "unsupported Debian target: $target" >&2
    exit 2
    ;;
esac

command -v dpkg-deb >/dev/null 2>&1 || {
  echo "dpkg-deb is required to generate a Debian package" >&2
  exit 1
}

binary="$dist_dir/omgb-$target"
plugin="$dist_dir/plugin-$target"
[[ -f $binary ]] || { echo "missing binary: $binary" >&2; exit 1; }
[[ -d $plugin ]] || { echo "missing plugin tree: $plugin" >&2; exit 1; }
if find "$plugin" -type l -print -quit | grep -q .; then
  echo "plugin tree must not contain symbolic links" >&2
  exit 1
fi

# Escape the replacement tilde. In Bash parameter substitution an unescaped
# leading `~` expands to $HOME, producing an invalid path/version in CI.
deb_version=${version/-/\~}
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
package_root="$work_dir/omgb"

install -Dm755 "$binary" "$package_root/usr/lib/omgb/omgb"
mkdir -p "$package_root/usr/lib/omgb/plugin" "$package_root/usr/bin" "$package_root/DEBIAN"
cp -a "$plugin/." "$package_root/usr/lib/omgb/plugin/"
ln -s ../lib/omgb/omgb "$package_root/usr/bin/omgb"

installed_size=$(du -sk "$package_root/usr" | awk '{print $1}')
cat >"$package_root/DEBIAN/control" <<EOF
Package: omgb
Version: $deb_version
Section: devel
Priority: optional
Architecture: $deb_arch
Installed-Size: $installed_size
Maintainer: OMGB maintainers <josepha-mayo@users.noreply.github.com>
Depends: ca-certificates, libc6 (>= 2.31)
Homepage: https://github.com/josepha-mayo/oh-my-grok-build
Description: Productivity and mobile-relay harness for Grok Build
 OMGB adds providers, scheduling, subagents, team isolation, connectors,
 secure updates, and a local-first mobile relay to the Grok Build core.
EOF

output="$dist_dir/omgb_${deb_version}_${deb_arch}.deb"
dpkg-deb --build --root-owner-group "$package_root" "$output"
echo "$output"
