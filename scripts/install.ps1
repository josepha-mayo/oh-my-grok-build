[CmdletBinding()]
param(
    [string]$Version = $env:OMGB_VERSION,
    [string]$InstallRoot = $(if ($env:OMGB_INSTALL_ROOT) { $env:OMGB_INSTALL_ROOT } else { Join-Path $env:LOCALAPPDATA 'omgb' }),
    [switch]$Insecure,
    [switch]$NoPathUpdate
)

$ErrorActionPreference = 'Stop'
$repo = 'josepha-mayo/oh-my-grok-build'
$workflow = 'josepha-mayo/oh-my-grok-build/.github/workflows/release.yml'
$target = 'x86_64-pc-windows-msvc'

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($architecture -ne 'X64') {
    throw "Unsupported Windows architecture: $architecture"
}
if (-not $Version) {
    $release = Invoke-RestMethod -Headers @{ Accept = 'application/vnd.github+json' } -Uri "https://api.github.com/repos/$repo/releases/latest"
    $Version = [string]$release.tag_name
}
if ($Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$') {
    throw 'Release version must look like vMAJOR.MINOR.PATCH.'
}

$archive = "omgb-$target.tar.gz"
$baseUrl = "https://github.com/$repo/releases/download/$Version"
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("omgb-install-" + [guid]::NewGuid().ToString('N'))
$extractRoot = Join-Path $tempRoot 'extract'
New-Item -ItemType Directory -Force -Path $extractRoot | Out-Null

try {
    $archivePath = Join-Path $tempRoot $archive
    $checksumsPath = Join-Path $tempRoot 'checksums-sha256.txt'
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$archive" -OutFile $archivePath
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/checksums-sha256.txt" -OutFile $checksumsPath

    $expected = $null
    foreach ($line in Get-Content -LiteralPath $checksumsPath) {
        if ($line -match '^([0-9A-Fa-f]{64})\s+\*?(.+)$' -and $Matches[2] -eq $archive) {
            $expected = $Matches[1].ToLowerInvariant()
            break
        }
    }
    if (-not $expected) { throw "No SHA-256 checksum found for $archive." }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw "SHA-256 mismatch for $archive." }

    if ($Insecure -or $env:OMGB_INSTALL_INSECURE -eq '1') {
        Write-Warning 'Skipping build-provenance verification.'
    } else {
        if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
            throw 'GitHub CLI (gh) is required for provenance verification. Use -Insecure only if you accept the risk.'
        }
        & gh attestation verify $archivePath --repo $repo --signer-workflow $workflow --deny-self-hosted-runners
        if ($LASTEXITCODE -ne 0) { throw 'Build-provenance verification failed.' }
    }

    $archiveEntries = @(& tar.exe -tzf $archivePath)
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the release archive.' }
    foreach ($entry in $archiveEntries) {
        $normalizedEntry = ([string]$entry).Replace('\', '/')
        if ($normalizedEntry.StartsWith('/') -or $normalizedEntry -match '^[A-Za-z]:' -or ($normalizedEntry -split '/') -contains '..') {
            throw "Release archive contains an unsafe path: $entry"
        }
    }

    & tar.exe -xzf $archivePath -C $extractRoot
    if ($LASTEXITCODE -ne 0) { throw 'Could not extract the release archive.' }
    $links = Get-ChildItem -LiteralPath $extractRoot -Recurse -Force | Where-Object { $_.LinkType }
    if ($links) { throw 'Release archive contains a symbolic link.' }

    $binarySource = Join-Path $extractRoot "omgb-$target.exe"
    $pluginSource = Join-Path $extractRoot "plugin-$target"
    if (-not (Test-Path -LiteralPath $binarySource -PathType Leaf)) { throw 'Release archive has no omgb executable.' }
    if (-not (Test-Path -LiteralPath $pluginSource -PathType Container)) { throw 'Release archive has no plugin tree.' }

    $binRoot = Join-Path $InstallRoot 'bin'
    $pluginRoot = Join-Path $InstallRoot 'plugin'
    New-Item -ItemType Directory -Force -Path $binRoot, $pluginRoot | Out-Null
    Copy-Item -LiteralPath $binarySource -Destination (Join-Path $binRoot 'omgb.exe') -Force
    Copy-Item -Path (Join-Path $pluginSource '*') -Destination $pluginRoot -Recurse -Force

    if (-not $NoPathUpdate) {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @($userPath -split ';' | Where-Object { $_ })
        if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $binRoot.TrimEnd('\') })) {
            [Environment]::SetEnvironmentVariable('Path', (($entries + $binRoot) -join ';'), 'User')
            Write-Host "Added $binRoot to your user PATH. Open a new terminal to use it."
        }
    }

    Write-Host "Installed omgb $Version to $InstallRoot"
    & (Join-Path $binRoot 'omgb.exe') doctor
} finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}
