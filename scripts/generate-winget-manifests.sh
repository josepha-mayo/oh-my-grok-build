#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 5 ]]; then
  echo "usage: $0 MSI VERSION TAG PACKAGE_IDENTIFIER OUTPUT_DIR" >&2
  exit 2
fi

msi=$1
version=$2
tag=$3
package_id=$4
output_dir=$5

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "WinGet manifests require a stable numeric version: $version" >&2
  exit 2
fi
[[ $tag == "v$version" ]] || { echo "tag does not match version: $tag" >&2; exit 2; }
if [[ ! $package_id =~ ^[A-Za-z0-9_-]+\.[A-Za-z0-9_.-]+$ ]]; then
  echo "invalid WinGet package identifier: $package_id" >&2
  exit 2
fi
[[ -f $msi ]] || { echo "missing MSI: $msi" >&2; exit 1; }
command -v sha256sum >/dev/null 2>&1 || { echo "sha256sum is required" >&2; exit 1; }

mkdir -p "$output_dir"
filename=$(basename "$msi")
sha256=$(sha256sum <"$msi" | awk '{print toupper($1)}')
url="https://github.com/josepha-mayo/oh-my-grok-build/releases/download/$tag/$filename"

cat >"$output_dir/$package_id.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.1.12.0.schema.json
PackageIdentifier: $package_id
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.12.0
EOF

cat >"$output_dir/$package_id.installer.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.1.12.0.schema.json
PackageIdentifier: $package_id
PackageVersion: $version
InstallerType: msi
Scope: user
UpgradeBehavior: install
Installers:
  - Architecture: x64
    InstallerUrl: $url
    InstallerSha256: $sha256
ManifestType: installer
ManifestVersion: 1.12.0
EOF

cat >"$output_dir/$package_id.locale.en-US.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.1.12.0.schema.json
PackageIdentifier: $package_id
PackageVersion: $version
PackageLocale: en-US
Publisher: OMGB maintainers
PublisherUrl: https://github.com/josepha-mayo
PublisherSupportUrl: https://github.com/josepha-mayo/oh-my-grok-build/issues
PackageName: OMGB
PackageUrl: https://github.com/josepha-mayo/oh-my-grok-build
License: Apache-2.0
LicenseUrl: https://github.com/josepha-mayo/oh-my-grok-build/blob/$tag/LICENSE
ShortDescription: Productivity, orchestration, and mobile relay for Grok Build.
Moniker: omgb
Tags:
  - ai
  - cli
  - developer-tools
  - grok
  - orchestration
ReleaseNotesUrl: https://github.com/josepha-mayo/oh-my-grok-build/releases/tag/$tag
ManifestType: defaultLocale
ManifestVersion: 1.12.0
EOF

printf '%s\n' \
  "$output_dir/$package_id.yaml" \
  "$output_dir/$package_id.installer.yaml" \
  "$output_dir/$package_id.locale.en-US.yaml"
