[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\OhMyGrokBuild')
)

$ErrorActionPreference = 'Stop'
$repo = 'josepha-mayo/oh-my-grok-build'
$assetName = 'omgb-windows-x64.zip'
$checksumName = "$assetName.sha256"
$headers = @{ 'User-Agent' = 'oh-my-grok-build-installer' }
$release = Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$repo/releases/latest"
$asset = $release.assets | Where-Object name -eq $assetName | Select-Object -First 1
$checksumAsset = $release.assets | Where-Object name -eq $checksumName | Select-Object -First 1
if (-not $asset -or -not $checksumAsset) {
    throw "Latest release does not contain $assetName and $checksumName"
}

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("omgb-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tempRoot | Out-Null
try {
    $archive = Join-Path $tempRoot $assetName
    $checksumFile = Join-Path $tempRoot $checksumName
    Invoke-WebRequest -Headers $headers -Uri $asset.browser_download_url -OutFile $archive
    Invoke-WebRequest -Headers $headers -Uri $checksumAsset.browser_download_url -OutFile $checksumFile
    $expected = ((Get-Content -LiteralPath $checksumFile -Raw).Trim() -split '\s+')[0].ToUpperInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToUpperInvariant()
    if ($actual -ne $expected) { throw "Checksum mismatch for $assetName" }
    $expanded = Join-Path $tempRoot 'expanded'
    Expand-Archive -LiteralPath $archive -DestinationPath $expanded
    $source = Get-ChildItem -LiteralPath $expanded -Filter omgb.exe -File -Recurse | Select-Object -First 1
    if (-not $source) { throw 'Release archive does not contain omgb.exe' }
    $pluginSource = Join-Path $expanded 'plugin'
    if (-not (Test-Path -LiteralPath $pluginSource -PathType Container)) {
        throw 'Release archive does not contain the OMGB plugin tree'
    }
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -LiteralPath $source.FullName -Destination (Join-Path $InstallDir 'omgb.exe') -Force
    $pluginDestination = Join-Path $InstallDir 'plugin'
    if (Test-Path -LiteralPath $pluginDestination) {
        Remove-Item -LiteralPath $pluginDestination -Recurse -Force
    }
    Copy-Item -LiteralPath $pluginSource -Destination $pluginDestination -Recurse
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @($userPath -split ';' | Where-Object { $_ })
    if ($parts -notcontains $InstallDir) {
        [Environment]::SetEnvironmentVariable('Path', (($parts + $InstallDir) -join ';'), 'User')
    }
    & (Join-Path $InstallDir 'omgb.exe') --version
    Write-Host "Installed omgb to $InstallDir"
    Write-Host 'Open a new terminal, then run: omgb'
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
