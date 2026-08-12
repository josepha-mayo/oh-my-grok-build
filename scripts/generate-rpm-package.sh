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
  x86_64-unknown-linux-gnu) rpm_arch=x86_64 ;;
  aarch64-unknown-linux-gnu) rpm_arch=aarch64 ;;
  *)
    echo "unsupported RPM target: $target" >&2
    exit 2
    ;;
esac

command -v rpmbuild >/dev/null 2>&1 || {
  echo "rpmbuild is required to generate an RPM package" >&2
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

rpm_version=${version%%[-+]*}
rpm_release=1
if [[ $version == *-* ]]; then
  prerelease=${version#*-}
  prerelease=${prerelease%%+*}
  rpm_release="0.${prerelease//-/.}"
fi

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
top_dir="$work_dir/rpmbuild"
payload="$work_dir/payload"
mkdir -p "$top_dir"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

install -Dm755 "$binary" "$payload/usr/lib/omgb/omgb"
mkdir -p "$payload/usr/lib/omgb/plugin" "$payload/usr/bin"
cp -a "$plugin/." "$payload/usr/lib/omgb/plugin/"
ln -s ../lib/omgb/omgb "$payload/usr/bin/omgb"
tar --owner=0 --group=0 -czf "$top_dir/SOURCES/omgb-payload.tar.gz" -C "$payload" .

cat >"$top_dir/SPECS/omgb.spec" <<EOF
Name: omgb
Version: $rpm_version
Release: $rpm_release%{?dist}
Summary: Productivity and mobile-relay harness for Grok Build
License: Apache-2.0
URL: https://github.com/josepha-mayo/oh-my-grok-build
Source0: omgb-payload.tar.gz
BuildArch: $rpm_arch
Requires: ca-certificates

%description
OMGB adds providers, scheduling, subagents, team isolation, connectors,
secure updates, and a local-first mobile relay to the Grok Build core.

%prep

%build

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}
tar -xzf %{SOURCE0} -C %{buildroot}

%files
/usr/bin/omgb
/usr/lib/omgb

%changelog
EOF

rpmbuild --define "_topdir $top_dir" -bb "$top_dir/SPECS/omgb.spec"
package=$(find "$top_dir/RPMS/$rpm_arch" -maxdepth 1 -type f -name 'omgb-*.rpm' -print -quit)
[[ -n $package ]] || { echo "rpmbuild did not produce a package" >&2; exit 1; }
cp "$package" "$dist_dir/"
echo "$dist_dir/$(basename "$package")"
