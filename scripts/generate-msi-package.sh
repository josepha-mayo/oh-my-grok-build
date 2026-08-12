#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 WINDOWS_ARCHIVE VERSION OUTPUT_DIR" >&2
  exit 2
fi

archive=$1
version=$2
output_dir=$3
target=x86_64-pc-windows-msvc

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid semantic version: $version" >&2
  exit 2
fi

for tool in tar wixl wixl-heat msiextract; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "$tool is required to generate and validate an MSI package" >&2
    exit 1
  }
done
[[ -f $archive ]] || { echo "missing Windows archive: $archive" >&2; exit 1; }

msi_version=${version%%[-+]*}
IFS=. read -r major minor patch <<<"$msi_version"
if (( major > 255 || minor > 255 || patch > 65535 )); then
  echo "version exceeds Windows Installer limits: $msi_version" >&2
  exit 2
fi

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
extracted="$work_dir/extracted"
payload="$work_dir/payload"
mkdir -p "$extracted" "$payload/plugin" "$output_dir"

while IFS= read -r entry; do
  case "$entry" in
    /*|[A-Za-z]:*|../*|*/../*|*/..)
      echo "unsafe path in Windows archive: $entry" >&2
      exit 1
      ;;
  esac
done < <(tar -tzf "$archive")
tar -xzf "$archive" -C "$extracted"
binary="$extracted/omgb-$target.exe"
guard="$extracted/safe-shell-guard-$target.exe"
plugin="$extracted/plugin-$target"
[[ -f $binary ]] || { echo "archive is missing $binary" >&2; exit 1; }
[[ -f $guard ]] || { echo "archive is missing $guard" >&2; exit 1; }
[[ -d $plugin ]] || { echo "archive is missing $plugin" >&2; exit 1; }
if find "$extracted" -type l -print -quit | grep -q .; then
  echo "Windows archive must not contain symbolic links" >&2
  exit 1
fi

install -m755 "$binary" "$payload/omgb.exe"
install -m755 "$guard" "$payload/safe-shell-guard.exe"
cp -a "$plugin/." "$payload/plugin/"

find "$payload" -type f -print | LC_ALL=C sort |
  wixl-heat -p "$payload" --component-group CG.omgb --var var.SourceDir \
    --directory-ref INSTALLDIR --win64 >"$work_dir/files.wxs"

cat >"$work_dir/omgb.wxs" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Product Id="*" Name="OMGB" Language="1033" Version="$msi_version"
    Manufacturer="OMGB maintainers" UpgradeCode="5C052871-33D0-4D0E-982E-70E75DEAA6AF">
    <Package InstallerVersion="500" Compressed="yes" InstallScope="perUser"
      Description="Productivity and mobile-relay harness for Grok Build" />
    <MediaTemplate EmbedCab="yes" />
    <MajorUpgrade AllowSameVersionUpgrades="yes"
      DowngradeErrorMessage="A newer version of OMGB is already installed." />
    <Property Id="ARPCOMMENTS" Value="Productivity, orchestration, and mobile relay for Grok Build" />
    <Property Id="ARPHELPLINK" Value="https://github.com/josepha-mayo/oh-my-grok-build" />
    <Directory Id="TARGETDIR" Name="SourceDir">
      <Directory Id="LocalAppDataFolder">
        <Directory Id="LocalProgramsFolder" Name="Programs">
          <Directory Id="INSTALLDIR" Name="OMGB">
            <Component Id="PathEnvironment" Guid="D41DAFA5-6206-44A8-A69E-9EAA3B236C23" Win64="yes">
              <Environment Id="OmgbPath" Name="PATH" Value="[INSTALLDIR]" Permanent="no"
                Part="last" Action="set" System="no" />
            </Component>
          </Directory>
        </Directory>
      </Directory>
    </Directory>
    <Feature Id="Complete" Title="OMGB" Level="1">
      <ComponentGroupRef Id="CG.omgb" />
      <ComponentRef Id="PathEnvironment" />
    </Feature>
  </Product>
</Wix>
EOF

output="$output_dir/omgb-$version-x86_64.msi"
wixl --arch x64 -D SourceDir="$payload" -o "$output" "$work_dir/omgb.wxs" "$work_dir/files.wxs"
msiextract -l "$output" >/dev/null
echo "$output"
