use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

const REPO: &str = "josepha-mayo/oh-my-grok-build";
const SIGNER_WORKFLOW: &str = "josepha-mayo/oh-my-grok-build/.github/workflows/release.yml";
const MAX_CHECKSUMS_BYTES: usize = 1024 * 1024;
const MAX_RELEASE_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;
const MAX_RELEASE_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_UNPACKED_RELEASE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_RELEASE_TRANSACTION_JOURNAL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RELEASE_TRANSACTION_ENTRIES: usize = MAX_RELEASE_ARCHIVE_ENTRIES;

pub async fn run(args: &crate::args::SelfUpdateArgs) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let current_exe = std::env::current_exe().context("cannot locate current executable")?;
    let current_dir = current_exe.parent().unwrap_or(std::path::Path::new("."));
    recover_incomplete_release_transactions(current_dir, &current_exe)?;
    let target = current_target()?;
    let archive_name = format!("omgb-{target}.tar.gz");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent(format!("omgb/{current}"))
        .build()?;

    let release = fetch_release(&client, args.channel).await?;

    let tag = release["tag_name"]
        .as_str()
        .context("release has no tag_name")?;
    let assets = release["assets"]
        .as_array()
        .context("release has no assets")?;

    let new_version = tag.strip_prefix('v').unwrap_or(tag);
    let latest_version = semver::Version::parse(new_version)
        .with_context(|| format!("release tag {tag:?} is not valid SemVer"))?;
    let current_version = semver::Version::parse(current)
        .context("the installed omgb version is not valid SemVer")?;
    let archive = find_asset(assets, &archive_name)?;
    let archive_url = archive["browser_download_url"]
        .as_str()
        .context("archive asset has no url")?;
    let checksums = find_asset(assets, "checksums-sha256.txt")?;
    let checksums_url = checksums["browser_download_url"]
        .as_str()
        .context("checksums asset has no url")?;

    println!("Current version: {current}");
    println!("Latest {} release: {tag} ({})", args.channel, new_version);
    println!("Archive: {archive_url}");

    if latest_version == current_version {
        println!("Already up to date.");
        return Ok(());
    }
    if latest_version < current_version {
        println!("Skipping older release {tag}; installed version is {current}.");
        return Ok(());
    }

    if args.check || !args.apply {
        println!("Run with --apply to install this release.");
        return Ok(());
    }

    let tmp = tempfile::Builder::new()
        .prefix("omgb-update-")
        .tempdir()
        .context("failed to create temporary update directory")?;
    let archive_path = tmp.path().join(&archive_name);

    let checksums_text =
        String::from_utf8(download(&client, checksums_url, MAX_CHECKSUMS_BYTES).await?)
            .context("checksums file is not utf-8")?;
    let expected = parse_checksum(&checksums_text, &archive_name)?;

    let archive_bytes = download(&client, archive_url, MAX_RELEASE_ARCHIVE_BYTES).await?;
    std::fs::write(&archive_path, &archive_bytes)
        .with_context(|| format!("failed to write archive to {}", archive_path.display()))?;
    let actual = to_hex(&Sha256::digest(&archive_bytes));
    if actual != expected {
        bail!("checksum mismatch for {archive_name}: expected {expected}, got {actual}");
    }

    if args.insecure {
        eprintln!("Warning: --insecure skips build-provenance attestation verification.");
    } else {
        verify_attestation(&archive_path)?;
    }

    unpack_archive(&archive_path, tmp.path())?;

    let ext = if std::env::consts::OS == "windows" {
        ".exe"
    } else {
        ""
    };
    let extracted_bin = tmp.path().join(format!("omgb-{target}{ext}"));
    require_regular_file(&extracted_bin, "release binary")?;

    // Validate every destination the plugin overlay will touch before replacing
    // the executable. This avoids leaving a partly updated installation when a
    // local plugin path is malformed or symlinked.
    let guard_name = format!("safe-shell-guard{ext}");
    let guard_src = tmp.path().join(format!("safe-shell-guard-{target}{ext}"));
    let plugin_src = tmp.path().join(format!("plugin-{target}"));
    let plugin_root = find_plugin_root(current_dir);
    if let Some(plugin_root) = &plugin_root {
        if plugin_src.is_dir() {
            validate_release_tree(&plugin_src, plugin_root)?;
        } else if guard_src.exists() {
            require_regular_file(&guard_src, "release safe-shell-guard")?;
            validate_release_file_destination(&plugin_root.join("bin").join(&guard_name))?;
        }
    } else if guard_src.exists() {
        require_regular_file(&guard_src, "release safe-shell-guard")?;
        validate_release_file_destination(&current_dir.join(&guard_name))?;
    }

    let mut overlay_files = Vec::new();
    let overlay_label = if let Some(plugin_root) = &plugin_root {
        if plugin_src.is_dir() {
            collect_release_files(&plugin_src, plugin_root, &mut overlay_files)?;
            Some(format!("plugin {}", plugin_root.display()))
        } else if guard_src.exists() {
            overlay_files.push((guard_src.clone(), plugin_root.join("bin").join(&guard_name)));
            Some(format!("plugin {}", plugin_root.display()))
        } else {
            None
        }
    } else if guard_src.exists() {
        overlay_files.push((guard_src.clone(), current_dir.join(&guard_name)));
        Some(format!("{} in {}", guard_name, current_dir.display()))
    } else {
        None
    };
    let overlay_count = overlay_files.len();
    let install_root = release_transaction_install_root(current_dir, &overlay_files)?;
    let mut transaction =
        ReleaseTransaction::prepare(&install_root, &current_exe, &extracted_bin, &overlay_files)?;

    // Unix can replace a running executable directly. Windows stages the new
    // binary and starts a detached helper that swaps it in after this process exits.
    let (replaced, replacement_scheduled) = match transaction.replace_binary() {
        Ok(()) => {
            println!("Updated {} to {}", current_exe.display(), tag);
            (true, false)
        }
        Err(e) => {
            let staged = transaction.new_binary();
            println!("Could not replace running binary ({}).", e);
            #[cfg(windows)]
            {
                match schedule_windows_replacement(&staged, &current_exe, transaction.root()) {
                    Ok(()) => {
                        println!(
                            "Scheduled {} to replace {} after omgb exits.",
                            staged.display(),
                            current_exe.display()
                        );
                        (false, true)
                    }
                    Err(schedule_error) => {
                        eprintln!("Could not schedule automatic replacement: {schedule_error}");
                        println!(
                            "Staged update at {}. Close omgb and replace {} manually.",
                            staged.display(),
                            current_exe.display()
                        );
                        (false, false)
                    }
                }
            }
            #[cfg(not(windows))]
            {
                println!(
                    "Staged update at {}. Close omgb and replace {} manually.",
                    staged.display(),
                    current_exe.display()
                );
                (false, false)
            }
        }
    };

    if replaced {
        if let Err(error) = transaction.apply_overlay() {
            transaction.rollback()?;
            return Err(error);
        }
        transaction.commit()?;
        if overlay_count > 0 {
            println!(
                "Updated {} ({overlay_count} managed file(s))",
                overlay_label.as_deref().unwrap_or("release plugin")
            );
        }
    } else if !replacement_scheduled {
        transaction.rollback()?;
        println!("Kept the installed plugin unchanged until the staged binary is installed.");
    }
    Ok(())
}

fn windows_replacement_script() -> &'static str {
    r#"$ErrorActionPreference = 'Stop'
$source = $env:OMGB_UPDATE_SOURCE
$destination = $env:OMGB_UPDATE_DESTINATION
$transaction = $env:OMGB_UPDATE_TRANSACTION
$parentId = [int]$env:OMGB_UPDATE_PARENT_PID
$errorPath = "$source.error.log"
function Resolve-TransactionPath($root, $relative) {
    $fullRoot = [System.IO.Path]::GetFullPath($root).TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    $fullPath = [System.IO.Path]::GetFullPath((Join-Path $fullRoot $relative))
    if (-not $fullPath.StartsWith("$fullRoot$([System.IO.Path]::DirectorySeparatorChar)", [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'Release transaction path escapes its root.'
    }
    return $fullPath
}
try {
    for ($attempt = 0; $attempt -lt 600; $attempt++) {
        if (-not (Get-Process -Id $parentId -ErrorAction SilentlyContinue)) { break }
        Start-Sleep -Milliseconds 200
    }
    if (Get-Process -Id $parentId -ErrorAction SilentlyContinue) {
        throw 'Timed out waiting for omgb to exit.'
    }
    for ($attempt = 0; $attempt -lt 50; $attempt++) {
        try {
            if (Test-Path -LiteralPath $destination) {
                $backupPath = "$destination.omgb-backup-$([Guid]::NewGuid().ToString('N'))"
                try {
                    [System.IO.File]::Replace($source, $destination, $backupPath, $true)
                    try { [System.IO.File]::Delete($backupPath) } catch {}
                } catch {
                    try { [System.IO.File]::Delete($backupPath) } catch {}
                    throw
                }
            } else {
                Move-Item -LiteralPath $source -Destination $destination
            }
            $journalPath = Join-Path $transaction 'journal.json'
            $journal = Get-Content -LiteralPath $journalPath -Raw | ConvertFrom-Json
            $installRoot = Split-Path -Parent $transaction
            foreach ($entry in $journal.entries) {
                $entryDestination = Resolve-TransactionPath $installRoot $entry.destination
                $parent = Split-Path -Parent $entryDestination
                [System.IO.Directory]::CreateDirectory($parent) | Out-Null
                $temporary = "$entryDestination.omgb-new-$([Guid]::NewGuid().ToString('N'))"
                [System.IO.File]::Copy((Resolve-TransactionPath $transaction $entry.staged), $temporary, $true)
                if (Test-Path -LiteralPath $entryDestination) {
                    [System.IO.File]::Replace($temporary, $entryDestination, $null, $true)
                } else {
                    Move-Item -LiteralPath $temporary -Destination $entryDestination
                }
            }
            $journal.state = 'committed'
            $journalJson = $journal | ConvertTo-Json -Depth 4
            [System.IO.File]::WriteAllText($journalPath, $journalJson, [System.Text.Encoding]::UTF8)
            Remove-Item -LiteralPath $transaction -Recurse -Force
            if (Test-Path -LiteralPath $errorPath) {
                [System.IO.File]::Delete($errorPath)
            }
            exit 0
        } catch {
            if ($attempt -eq 49) { throw }
            Start-Sleep -Milliseconds 200
        }
    }
} catch {
    try {
        $journalPath = Join-Path $transaction 'journal.json'
        $journal = Get-Content -LiteralPath $journalPath -Raw | ConvertFrom-Json
        $installRoot = Split-Path -Parent $transaction
        [System.IO.File]::Copy((Resolve-TransactionPath $transaction $journal.binary_backup), $destination, $true)
        foreach ($entry in $journal.entries) {
            $entryDestination = Resolve-TransactionPath $installRoot $entry.destination
            if ($null -ne $entry.backup) {
                [System.IO.File]::Copy((Resolve-TransactionPath $transaction $entry.backup), $entryDestination, $true)
            } elseif (Test-Path -LiteralPath $entryDestination) {
                [System.IO.File]::Delete($entryDestination)
            }
        }
    } catch {}
    [System.IO.File]::WriteAllText($errorPath, $_.Exception.Message)
    exit 1
}"#
}

#[cfg(windows)]
fn schedule_windows_replacement(
    staged: &Path,
    destination: &Path,
    transaction: &Path,
) -> Result<()> {
    schedule_windows_replacement_after(staged, destination, transaction, std::process::id())
        .map(|_| ())
}

#[cfg(windows)]
fn schedule_windows_replacement_after(
    staged: &Path,
    destination: &Path,
    transaction: &Path,
    parent_pid: u32,
) -> Result<std::process::Child> {
    use base64::Engine;
    use std::os::windows::process::CommandExt;

    let utf16: Vec<u8> = windows_replacement_script()
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-EncodedCommand",
            &encoded,
        ])
        .env("OMGB_UPDATE_SOURCE", staged)
        .env("OMGB_UPDATE_DESTINATION", destination)
        .env("OMGB_UPDATE_TRANSACTION", transaction)
        .env("OMGB_UPDATE_PARENT_PID", parent_pid.to_string())
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()
        .context("failed to start the Windows update replacement helper")
}

fn current_target() -> Result<&'static str> {
    target_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn target_for(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        (os, arch) => bail!("unsupported platform for self-update: {os}/{arch}"),
    }
}

async fn fetch_release(
    client: &reqwest::Client,
    channel: crate::args::UpdateChannel,
) -> Result<serde_json::Value> {
    match channel {
        crate::args::UpdateChannel::Stable => {
            let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
            let response = client.get(&url).send().await?;
            ensure_release_status(response.status(), channel)?;
            response
                .json()
                .await
                .context("failed to parse latest release JSON")
        }
        crate::args::UpdateChannel::Nightly => {
            let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=20");
            let response = client.get(&url).send().await?;
            ensure_release_status(response.status(), channel)?;
            let releases: Vec<serde_json::Value> = response
                .json()
                .await
                .context("failed to parse releases JSON")?;
            releases
                .into_iter()
                .find(|r| r["prerelease"].as_bool().unwrap_or(false))
                .context("no pre-release found for nightly channel")
        }
    }
}

fn ensure_release_status(
    status: reqwest::StatusCode,
    channel: crate::args::UpdateChannel,
) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    if status == reqwest::StatusCode::NOT_FOUND
        && matches!(channel, crate::args::UpdateChannel::Stable)
    {
        bail!(
            "no published stable release is available yet; a maintainer must publish a GitHub Release before omgb can update"
        );
    }
    bail!("GitHub Releases API returned HTTP {status} for the {channel} channel")
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn find_asset<'a>(assets: &'a [serde_json::Value], name: &str) -> Result<&'a serde_json::Value> {
    assets
        .iter()
        .find(|a| a["name"].as_str() == Some(name))
        .with_context(|| format!("asset {name} not found in release"))
}

async fn download(client: &reqwest::Client, url: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        bail!("release asset exceeds the {max_bytes}-byte download limit");
    }

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("failed to download asset")?;
        append_download_chunk(&mut bytes, &chunk, max_bytes)?;
    }
    Ok(bytes)
}

fn append_download_chunk(bytes: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> Result<()> {
    if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
        bail!("release asset exceeds the {max_bytes}-byte download limit");
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

fn parse_checksum(text: &str, name: &str) -> Result<String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if let Some(hash) = parts.next()
            && let Some(filename) = parts.next()
            && filename.trim_start_matches('*') == name
        {
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("invalid SHA-256 checksum for {name}");
            }
            return Ok(hash.to_ascii_lowercase());
        }
    }
    bail!("no checksum found for {name}")
}

fn verify_attestation(archive_path: &Path) -> Result<()> {
    let gh = which::which("gh").map_err(|_| {
        anyhow::anyhow!(
            "GitHub CLI (gh) is required to verify build provenance. \
             Install it from https://cli.github.com or re-run with --insecure (not recommended)."
        )
    })?;

    let output = std::process::Command::new(&gh)
        .arg("attestation")
        .arg("verify")
        .arg(archive_path)
        .arg("--repo")
        .arg(REPO)
        .arg("--signer-workflow")
        .arg(SIGNER_WORKFLOW)
        .arg("--deny-self-hosted-runners")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .with_context(|| format!("failed to run {}", gh.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("attestation verification failed: {stderr}");
    }

    println!("Build-provenance attestation verified.");
    Ok(())
}

fn unpack_archive(archive_path: &Path, destination: &Path) -> Result<()> {
    let archive_file = std::fs::File::open(archive_path)
        .with_context(|| format!("failed to open {}", archive_path.display()))?;
    let decoder = flate2::read::GzDecoder::new(archive_file);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = 0;
    let mut unpacked_bytes = 0_u64;

    for entry in archive
        .entries()
        .context("failed to read release archive")?
    {
        let mut entry = entry.context("failed to read release archive entry")?;
        entries += 1;
        if entries > MAX_RELEASE_ARCHIVE_ENTRIES {
            bail!("release archive contains too many entries");
        }
        let path = entry
            .path()
            .context("release archive entry has an invalid path")?
            .to_path_buf();
        if !is_safe_archive_path(&path) {
            bail!(
                "release archive contains an unsafe path: {}",
                path.display()
            );
        }
        let entry_type = entry.header().entry_type();
        if !(entry_type.is_file() || entry_type.is_dir()) {
            bail!(
                "release archive contains unsupported entry type for {}",
                path.display()
            );
        }
        if entry_type.is_file() {
            unpacked_bytes = unpacked_bytes
                .checked_add(entry.size())
                .context("release archive unpacked size overflow")?;
            if unpacked_bytes > MAX_UNPACKED_RELEASE_BYTES {
                bail!("release archive exceeds the unpacked size limit");
            }
        }
        if !entry
            .unpack_in(destination)
            .with_context(|| format!("failed to extract {}", path.display()))?
        {
            bail!(
                "release archive path escapes destination: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn is_safe_archive_path(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn find_plugin_root(current_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let candidates = [
        Some(current_dir.join("plugin")),
        current_dir.parent().map(|p| p.join("plugin")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_dir() && p.join("bin").is_dir())
}

fn require_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} is not a regular file: {}", path.display());
    }
    Ok(())
}

fn validate_release_tree(source: &Path, destination: &Path) -> Result<()> {
    let source_meta = std::fs::symlink_metadata(source).with_context(|| {
        format!(
            "failed to inspect release plugin source {}",
            source.display()
        )
    })?;
    if source_meta.file_type().is_symlink() || !source_meta.is_dir() {
        bail!(
            "release plugin source is not a regular directory: {}",
            source.display()
        );
    }
    validate_release_directory(destination)?;

    for entry in std::fs::read_dir(source).with_context(|| {
        format!(
            "failed to read release plugin directory {}",
            source.display()
        )
    })? {
        let entry = entry.context("failed to read release plugin entry")?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_path).with_context(|| {
            format!(
                "failed to inspect release plugin entry {}",
                source_path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            bail!(
                "release plugin contains a symlink: {}",
                source_path.display()
            );
        }
        if metadata.is_dir() {
            match std::fs::symlink_metadata(&destination_path) {
                Ok(_) => validate_release_tree(&source_path, &destination_path)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    validate_release_tree_source(&source_path)?;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to inspect plugin directory {}",
                            destination_path.display()
                        )
                    });
                }
            }
        } else if metadata.is_file() {
            validate_release_file_destination(&destination_path)?;
        } else {
            bail!(
                "release plugin contains an unsupported entry: {}",
                source_path.display()
            );
        }
    }
    Ok(())
}

fn validate_release_tree_source(source: &Path) -> Result<()> {
    for entry in std::fs::read_dir(source).with_context(|| {
        format!(
            "failed to read release plugin directory {}",
            source.display()
        )
    })? {
        let entry = entry.context("failed to read release plugin entry")?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).with_context(|| {
            format!("failed to inspect release plugin entry {}", path.display())
        })?;
        if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
            bail!(
                "release plugin contains an unsafe entry: {}",
                path.display()
            );
        }
        if metadata.is_dir() {
            validate_release_tree_source(&path)?;
        }
    }
    Ok(())
}

fn validate_release_directory(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!(
                "plugin destination is not a regular directory: {}",
                path.display()
            );
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect plugin directory {}", path.display())),
    }
}

fn validate_release_file_destination(destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        validate_release_directory(parent)?;
    }
    match std::fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => bail!(
            "plugin destination is not a regular file: {}",
            destination.display()
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to inspect plugin destination {}",
                destination.display()
            )
        }),
    }
}

fn collect_release_files(
    source: &Path,
    destination: &Path,
    files: &mut Vec<(std::path::PathBuf, std::path::PathBuf)>,
) -> Result<()> {
    for entry in std::fs::read_dir(source).with_context(|| {
        format!(
            "failed to read release plugin directory {}",
            source.display()
        )
    })? {
        let entry = entry.context("failed to read release plugin entry")?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_path).with_context(|| {
            format!(
                "failed to inspect release plugin entry {}",
                source_path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            bail!(
                "release plugin contains a symlink: {}",
                source_path.display()
            );
        }
        if metadata.is_dir() {
            collect_release_files(&source_path, &destination_path, files)?;
        } else if metadata.is_file() {
            files.push((source_path, destination_path));
        } else {
            bail!(
                "release plugin contains an unsupported entry: {}",
                source_path.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
struct ReleaseOverlayEntry {
    destination: std::path::PathBuf,
    backup: Option<std::path::PathBuf>,
}

#[cfg(test)]
struct ReleaseOverlayTransaction {
    entries: Vec<ReleaseOverlayEntry>,
    applied: usize,
    committed: bool,
    _backup_dir: tempfile::TempDir,
}

#[cfg(test)]
impl ReleaseOverlayTransaction {
    fn apply(files: &[(std::path::PathBuf, std::path::PathBuf)]) -> Result<Self> {
        let backup_dir = tempfile::Builder::new()
            .prefix("omgb-plugin-backup-")
            .tempdir()
            .context("failed to create plugin rollback directory")?;
        let mut entries = Vec::with_capacity(files.len());
        for (index, (_, destination)) in files.iter().enumerate() {
            let backup = match std::fs::symlink_metadata(destination) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    bail!(
                        "plugin destination is not a regular file: {}",
                        destination.display()
                    )
                }
                Ok(_) => {
                    let backup = backup_dir.path().join(index.to_string());
                    std::fs::copy(destination, &backup).with_context(|| {
                        format!(
                            "failed to back up plugin file {} before update",
                            destination.display()
                        )
                    })?;
                    Some(backup)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            entries.push(ReleaseOverlayEntry {
                destination: destination.clone(),
                backup,
            });
        }

        let mut transaction = Self {
            entries,
            applied: 0,
            committed: false,
            _backup_dir: backup_dir,
        };
        for (index, (source, destination)) in files.iter().enumerate() {
            if let Err(update_error) = copy_release_file(source, destination) {
                transaction.applied = index + 1;
                if let Err(rollback_error) = transaction.rollback_inner() {
                    transaction.committed = true;
                    bail!(
                        "plugin update failed ({update_error}); rollback also failed ({rollback_error})"
                    );
                }
                transaction.committed = true;
                return Err(update_error);
            }
            transaction.applied = index + 1;
        }
        Ok(transaction)
    }

    fn rollback_inner(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for entry in self.entries[..self.applied].iter().rev() {
            let result = if let Some(backup) = &entry.backup {
                copy_release_file(backup, &entry.destination)
            } else {
                match std::fs::remove_file(&entry.destination) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error.into()),
                }
            };
            if let Err(error) = result {
                errors.push(format!("{}: {error}", entry.destination.display()));
            }
        }
        self.applied = 0;
        if errors.is_empty() {
            Ok(())
        } else {
            bail!("failed to restore plugin files: {}", errors.join("; "))
        }
    }
}

#[cfg(test)]
impl Drop for ReleaseOverlayTransaction {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.rollback_inner();
        }
    }
}

const RELEASE_TRANSACTION_DIR: &str = ".omgb-update-transaction";

#[derive(serde::Serialize, serde::Deserialize)]
struct ReleaseJournal {
    state: String,
    binary_backup: String,
    entries: Vec<ReleaseJournalEntry>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ReleaseJournalEntry {
    destination: String,
    backup: Option<String>,
    staged: String,
}

struct ReleaseTransaction {
    root: PathBuf,
    current_exe: PathBuf,
    journal: ReleaseJournal,
}

struct ReleaseTransactionPreparation {
    root: PathBuf,
    complete: bool,
}

impl Drop for ReleaseTransactionPreparation {
    fn drop(&mut self) {
        if !self.complete {
            let _ = remove_release_transaction(&self.root);
        }
    }
}

impl ReleaseTransaction {
    fn prepare(
        install_dir: &Path,
        current_exe: &Path,
        extracted_bin: &Path,
        files: &[(PathBuf, PathBuf)],
    ) -> Result<Self> {
        require_regular_file(current_exe, "installed executable")?;
        validate_release_directory(install_dir)?;
        let root = install_dir.join(RELEASE_TRANSACTION_DIR);
        if root.exists() {
            bail!(
                "an incomplete release transaction remains at {}",
                root.display()
            );
        }
        std::fs::create_dir(&root)
            .with_context(|| format!("failed to create release transaction {}", root.display()))?;
        crate::providers::restrict_omg_directory_permissions(&root)?;
        let mut cleanup = ReleaseTransactionPreparation {
            root: root.clone(),
            complete: false,
        };
        std::fs::create_dir(root.join("backup"))?;
        std::fs::create_dir(root.join("new"))?;
        crate::providers::restrict_omg_directory_permissions(&root.join("backup"))?;
        crate::providers::restrict_omg_directory_permissions(&root.join("new"))?;
        let binary_backup = "backup/binary".to_owned();
        copy_release_file_durable(current_exe, &root.join(&binary_backup))?;
        copy_release_file_durable(extracted_bin, &root.join("new/binary"))?;

        let mut entries = Vec::with_capacity(files.len());
        for (index, (source, destination)) in files.iter().enumerate() {
            let relative = transaction_relative_path(install_dir, destination)?;
            let backup = match std::fs::symlink_metadata(destination) {
                Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                    bail!(
                        "plugin destination is not a regular file: {}",
                        destination.display()
                    )
                }
                Ok(_) => {
                    let path = format!("backup/{index}");
                    copy_release_file_durable(destination, &root.join(&path))?;
                    Some(path)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            let staged = format!("new/{index}");
            copy_release_file_durable(source, &root.join(&staged))?;
            entries.push(ReleaseJournalEntry {
                destination: relative,
                backup,
                staged,
            });
        }
        let mut transaction = Self {
            root,
            current_exe: current_exe.to_path_buf(),
            journal: ReleaseJournal {
                state: "prepared".into(),
                binary_backup,
                entries,
            },
        };
        transaction.write_journal()?;
        transaction.journal.state = "applying".into();
        transaction.write_journal()?;
        cleanup.complete = true;
        Ok(transaction)
    }

    fn root(&self) -> &Path {
        &self.root
    }
    fn new_binary(&self) -> PathBuf {
        self.root.join("new/binary")
    }

    fn replace_binary(&mut self) -> Result<()> {
        copy_release_file_durable(&self.new_binary(), &self.current_exe)
    }

    fn apply_overlay(&mut self) -> Result<()> {
        for entry in &self.journal.entries {
            let destination = transaction_destination(
                self.root
                    .parent()
                    .context("transaction has no install directory")?,
                &entry.destination,
            )?;
            copy_release_file_durable(&self.root.join(&entry.staged), &destination)?;
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<()> {
        rollback_release_journal(
            self.root
                .parent()
                .context("transaction has no install directory")?,
            &self.current_exe,
            &self.root,
            &self.journal,
        )?;
        remove_release_transaction(&self.root)
    }

    fn commit(&mut self) -> Result<()> {
        self.journal.state = "committed".into();
        self.write_journal()?;
        remove_release_transaction(&self.root)
    }

    fn write_journal(&self) -> Result<()> {
        crate::providers::write_file_atomic(
            &self.root.join("journal.json"),
            serde_json::to_vec(&self.journal)?,
            true,
        )
    }
}

fn transaction_relative_path(install_dir: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(install_dir).with_context(|| {
        format!(
            "release destination escapes install directory: {}",
            path.display()
        )
    })?;
    if relative.as_os_str().is_empty() || !is_safe_archive_path(relative) {
        bail!("unsafe release transaction destination: {}", path.display());
    }
    Ok(relative.to_string_lossy().into_owned())
}

fn transaction_destination(install_dir: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty() || !is_safe_archive_path(relative) {
        bail!("unsafe release transaction path: {}", relative.display());
    }
    let destination = install_dir.join(relative);
    let mut ancestor = install_dir.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for component in &components[..components.len().saturating_sub(1)] {
        if let Component::Normal(name) = component {
            ancestor.push(name);
            validate_release_directory(&ancestor)?;
        }
    }
    validate_release_file_destination(&destination)?;
    Ok(destination)
}

fn recover_incomplete_release_transaction(install_dir: &Path, current_exe: &Path) -> Result<()> {
    let root = install_dir.join(RELEASE_TRANSACTION_DIR);
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            bail!("unsafe release transaction directory: {}", root.display())
        }
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }
    let journal_path =
        release_transaction_artifact(&root, "journal.json", "release transaction journal")?;
    if std::fs::metadata(&journal_path)?.len() > MAX_RELEASE_TRANSACTION_JOURNAL_BYTES {
        bail!("release transaction journal exceeds the size limit");
    }
    let journal: ReleaseJournal = serde_json::from_slice(&std::fs::read(journal_path)?)
        .context("invalid release transaction journal")?;
    if journal.state == "committed" {
        return remove_release_transaction(&root);
    }
    if !matches!(journal.state.as_str(), "prepared" | "applying") {
        bail!("unknown release transaction state: {}", journal.state);
    }
    if journal.entries.len() > MAX_RELEASE_TRANSACTION_ENTRIES {
        bail!("release transaction journal has too many entries");
    }
    let mut destinations = std::collections::HashSet::new();
    let mut artifacts = std::collections::HashSet::new();
    artifacts.insert(journal.binary_backup.as_str());
    for entry in &journal.entries {
        if !destinations.insert(entry.destination.as_str())
            || !artifacts.insert(entry.staged.as_str())
            || entry
                .backup
                .as_deref()
                .is_some_and(|backup| !artifacts.insert(backup))
        {
            bail!("release transaction journal contains duplicate paths");
        }
        let _ = transaction_destination(install_dir, &entry.destination)?;
        validate_staged_release_path(&entry.staged)?;
    }
    rollback_release_journal(install_dir, current_exe, &root, &journal)?;
    remove_release_transaction(&root)
}

fn recover_incomplete_release_transactions(current_dir: &Path, current_exe: &Path) -> Result<()> {
    recover_incomplete_release_transaction(current_dir, current_exe)?;
    if let Some(parent) = current_dir.parent()
        && parent != current_dir
    {
        recover_incomplete_release_transaction(parent, current_exe)?;
    }
    Ok(())
}

fn release_transaction_install_root(
    current_dir: &Path,
    files: &[(PathBuf, PathBuf)],
) -> Result<PathBuf> {
    if files
        .iter()
        .all(|(_, destination)| destination.starts_with(current_dir))
    {
        return Ok(current_dir.to_path_buf());
    }
    let parent = current_dir
        .parent()
        .context("release destinations are outside the executable directory")?;
    if files
        .iter()
        .all(|(_, destination)| destination.starts_with(parent))
    {
        Ok(parent.to_path_buf())
    } else {
        bail!("release destinations do not share a supported installation root")
    }
}

fn rollback_release_journal(
    install_dir: &Path,
    current_exe: &Path,
    root: &Path,
    journal: &ReleaseJournal,
) -> Result<()> {
    let binary_backup = release_transaction_artifact(
        root,
        &journal.binary_backup,
        "release transaction binary backup",
    )?;
    copy_release_file_durable(&binary_backup, current_exe)?;
    for entry in journal.entries.iter().rev() {
        let destination = transaction_destination(install_dir, &entry.destination)?;
        if let Some(backup) = &entry.backup {
            let backup =
                release_transaction_artifact(root, backup, "release transaction plugin backup")?;
            copy_release_file_durable(&backup, &destination)?;
        } else if destination.exists() {
            std::fs::remove_file(&destination).with_context(|| {
                format!(
                    "failed to remove updated plugin file {}",
                    destination.display()
                )
            })?;
        }
    }
    Ok(())
}

fn release_transaction_artifact(root: &Path, relative: &str, label: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty() || !is_safe_archive_path(relative) {
        bail!("unsafe {label} path: {}", relative.display());
    }
    let root = dunce::canonicalize(root)?;
    let path = root.join(relative);
    require_regular_file(&path, label)?;
    let canonical = dunce::canonicalize(&path)?;
    if !canonical.starts_with(&root) {
        bail!("unsafe {label} path: {}", path.display());
    }
    Ok(canonical)
}

fn validate_staged_release_path(relative: &str) -> Result<()> {
    let relative = Path::new(relative);
    let mut components = relative.components();
    if relative.as_os_str().is_empty()
        || !is_safe_archive_path(relative)
        || !matches!(components.next(), Some(Component::Normal(part)) if part == "new")
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        bail!(
            "unsafe staged release transaction path: {}",
            relative.display()
        );
    }
    Ok(())
}

fn remove_release_transaction(root: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(root)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        bail!("unsafe release transaction directory: {}", root.display());
    }
    std::fs::remove_dir_all(root)
        .with_context(|| format!("failed to remove release transaction {}", root.display()))
}

#[cfg(test)]
#[cfg(test)]
fn copy_release_tree(source: &Path, destination: &Path) -> Result<usize> {
    let source_meta = std::fs::symlink_metadata(source).with_context(|| {
        format!(
            "failed to inspect release plugin source {}",
            source.display()
        )
    })?;
    if source_meta.file_type().is_symlink() || !source_meta.is_dir() {
        bail!(
            "release plugin source is not a regular directory: {}",
            source.display()
        );
    }

    match std::fs::symlink_metadata(destination) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            bail!(
                "plugin destination is not a regular directory: {}",
                destination.display()
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(destination).with_context(|| {
                format!(
                    "failed to create plugin directory {}",
                    destination.display()
                )
            })?;
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect plugin directory {}",
                    destination.display()
                )
            });
        }
    }

    let mut copied = 0;
    for entry in std::fs::read_dir(source).with_context(|| {
        format!(
            "failed to read release plugin directory {}",
            source.display()
        )
    })? {
        let entry = entry.context("failed to read release plugin entry")?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_path).with_context(|| {
            format!(
                "failed to inspect release plugin entry {}",
                source_path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            bail!(
                "release plugin contains a symlink: {}",
                source_path.display()
            );
        }
        if metadata.is_dir() {
            copied += copy_release_tree(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            copy_release_file(&source_path, &destination_path)?;
            copied += 1;
        } else {
            bail!(
                "release plugin contains an unsupported entry: {}",
                source_path.display()
            );
        }
    }
    Ok(copied)
}

#[cfg(test)]
fn copy_release_file(source: &Path, destination: &Path) -> Result<()> {
    let source_meta = std::fs::symlink_metadata(source)
        .with_context(|| format!("failed to inspect release file {}", source.display()))?;
    if source_meta.file_type().is_symlink() || !source_meta.is_file() {
        bail!("release file is not a regular file: {}", source.display());
    }
    if let Some(parent) = destination.parent() {
        match std::fs::symlink_metadata(parent) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                bail!(
                    "plugin destination parent is not a regular directory: {}",
                    parent.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "failed to create plugin destination directory {}",
                        parent.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect plugin destination directory {}",
                        parent.display()
                    )
                });
            }
        }
    }
    if let Ok(metadata) = std::fs::symlink_metadata(destination)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        bail!(
            "plugin destination is not a regular file: {}",
            destination.display()
        );
    }
    let content = std::fs::read(source)
        .with_context(|| format!("failed to read release plugin file {}", source.display()))?;
    crate::providers::write_file_atomic(destination, content, false).with_context(|| {
        format!(
            "failed to publish release plugin file {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    std::fs::set_permissions(destination, source_meta.permissions()).with_context(|| {
        format!(
            "failed to preserve permissions for plugin file {}",
            destination.display()
        )
    })?;
    Ok(())
}

fn copy_release_file_durable(source: &Path, destination: &Path) -> Result<()> {
    let source_meta = std::fs::symlink_metadata(source)
        .with_context(|| format!("failed to inspect release file {}", source.display()))?;
    if source_meta.file_type().is_symlink() || !source_meta.is_file() {
        bail!("release file is not a regular file: {}", source.display());
    }
    if let Some(parent) = destination.parent() {
        validate_release_directory(parent)?;
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    if let Ok(metadata) = std::fs::symlink_metadata(destination)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        bail!(
            "release destination is not a regular file: {}",
            destination.display()
        );
    }
    crate::providers::write_file_atomic(destination, std::fs::read(source)?, true)?;
    std::fs::set_permissions(destination, source_meta.permissions())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_replacer_uses_environment_paths_and_atomic_replace() {
        let script = windows_replacement_script();
        assert!(script.contains("$env:OMGB_UPDATE_SOURCE"));
        assert!(script.contains("$env:OMGB_UPDATE_DESTINATION"));
        assert!(script.contains("$env:OMGB_UPDATE_TRANSACTION"));
        assert!(script.contains("[System.IO.File]::Replace"));
        assert!(script.contains("Get-Process -Id $parentId"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_replacer_atomically_replaces_a_staged_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("path with spaces");
        std::fs::create_dir(&root).unwrap();
        let staged = root.join("omgb.new.exe");
        let destination = root.join("omgb.exe");
        std::fs::write(&staged, b"new executable").unwrap();
        std::fs::write(&destination, b"old executable").unwrap();

        let transaction = root.join(RELEASE_TRANSACTION_DIR);
        std::fs::create_dir(&transaction).unwrap();
        std::fs::create_dir(transaction.join("backup")).unwrap();
        std::fs::write(transaction.join("backup/binary"), "old executable").unwrap();
        std::fs::write(
            transaction.join("journal.json"),
            r#"{"state":"applying","binary_backup":"backup/binary","entries":[]}"#,
        )
        .unwrap();
        let mut helper = schedule_windows_replacement_after(
            &staged,
            &destination,
            &transaction,
            i32::MAX as u32,
        )
        .unwrap();
        for _ in 0..300 {
            if let Some(status) = helper.try_wait().unwrap() {
                let error_path = root.join("omgb.new.exe.error.log");
                let detail = std::fs::read_to_string(error_path).unwrap_or_default();
                assert!(
                    status.success(),
                    "Windows replacement helper failed: {detail}"
                );
                assert!(!staged.exists());
                assert_eq!(std::fs::read(&destination).unwrap(), b"new executable");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = helper.kill();
        let _ = helper.wait();
        let error_path = root.join("omgb.new.exe.error.log");
        let detail = std::fs::read_to_string(error_path).unwrap_or_default();
        panic!("Windows replacement helper did not finish: {detail}");
    }

    #[test]
    fn release_target_mapping_covers_supported_platforms() {
        assert_eq!(
            target_for("linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            target_for("linux", "aarch64").unwrap(),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            target_for("macos", "aarch64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            target_for("macos", "x86_64").unwrap(),
            "x86_64-apple-darwin"
        );
        assert_eq!(
            target_for("windows", "x86_64").unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert!(target_for("linux", "arm").is_err());
    }

    #[test]
    fn updater_explains_when_no_stable_release_is_published() {
        let error = ensure_release_status(
            reqwest::StatusCode::NOT_FOUND,
            crate::args::UpdateChannel::Stable,
        )
        .unwrap_err();
        assert!(error.to_string().contains("no published stable release"));
        assert!(
            ensure_release_status(reqwest::StatusCode::OK, crate::args::UpdateChannel::Stable)
                .is_ok()
        );
    }

    #[test]
    fn parse_checksum_handles_binary_flag_and_regular_format() {
        let text = concat!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa *omgb-x86_64-unknown-linux-gnu.tar.gz\n",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  omgb-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            parse_checksum(text, "omgb-x86_64-unknown-linux-gnu.tar.gz").unwrap(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            parse_checksum(text, "omgb-aarch64-apple-darwin.tar.gz").unwrap(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }

    #[test]
    fn parse_checksum_is_case_insensitive_for_hash() {
        let text = "ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD *omgb-x86_64-unknown-linux-gnu.tar.gz";
        assert_eq!(
            parse_checksum(text, "omgb-x86_64-unknown-linux-gnu.tar.gz").unwrap(),
            "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd"
        );
    }

    #[test]
    fn parse_checksum_rejects_non_sha256_digests() {
        assert!(
            parse_checksum(
                "abc123 *omgb-x86_64-unknown-linux-gnu.tar.gz",
                "omgb-x86_64-unknown-linux-gnu.tar.gz"
            )
            .is_err()
        );
    }

    #[test]
    fn archive_paths_cannot_escape_the_update_directory() {
        assert!(is_safe_archive_path(Path::new(
            "plugin/bin/safe-shell-guard"
        )));
        assert!(!is_safe_archive_path(Path::new("../omgb")));
        assert!(!is_safe_archive_path(Path::new("/omgb")));
    }

    #[test]
    fn to_hex_is_lowercase() {
        assert_eq!(to_hex(&[0xAB, 0xCD, 0xEF]), "abcdef");
    }

    #[test]
    fn download_chunks_cannot_exceed_the_limit() {
        let mut bytes = Vec::new();
        append_download_chunk(&mut bytes, b"abc", 4).unwrap();
        assert!(append_download_chunk(&mut bytes, b"de", 4).is_err());
        assert_eq!(bytes, b"abc");
    }

    #[test]
    fn plugin_overlay_updates_managed_files_without_deleting_additions() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("release-plugin");
        let destination = temp.path().join("installed-plugin");
        std::fs::create_dir_all(source.join("commands")).unwrap();
        std::fs::create_dir_all(destination.join("commands")).unwrap();
        std::fs::write(source.join("commands/live.md"), "new live command").unwrap();
        std::fs::write(source.join("hooks.json"), "new hooks").unwrap();
        std::fs::write(destination.join("commands/live.md"), "old live command").unwrap();
        std::fs::write(destination.join("commands/custom.md"), "keep me").unwrap();

        assert_eq!(copy_release_tree(&source, &destination).unwrap(), 2);
        assert_eq!(
            std::fs::read_to_string(destination.join("commands/live.md")).unwrap(),
            "new live command"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("commands/custom.md")).unwrap(),
            "keep me"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("hooks.json")).unwrap(),
            "new hooks"
        );
    }

    #[test]
    fn plugin_overlay_failure_restores_every_managed_file() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("release");
        let destination = temp.path().join("installed");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        let first_source = source.join("first");
        let missing_source = source.join("missing");
        let first_destination = destination.join("first");
        let second_destination = destination.join("second");
        std::fs::write(&first_source, "new first").unwrap();
        std::fs::write(&first_destination, "old first").unwrap();
        std::fs::write(&second_destination, "old second").unwrap();

        let result = ReleaseOverlayTransaction::apply(&[
            (first_source, first_destination.clone()),
            (missing_source, second_destination.clone()),
        ]);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(first_destination).unwrap(),
            "old first"
        );
        assert_eq!(
            std::fs::read_to_string(second_destination).unwrap(),
            "old second"
        );
    }

    #[test]
    fn release_transaction_root_covers_sibling_plugin_layout() {
        let install = PathBuf::from("install");
        let bin = install.join("bin");
        let files = vec![(
            PathBuf::from("release/plugin.json"),
            install.join("plugin/plugin.json"),
        )];
        assert_eq!(
            release_transaction_install_root(&bin, &files).unwrap(),
            install
        );
        assert_eq!(
            release_transaction_install_root(
                &bin,
                &[(PathBuf::from("release/guard"), bin.join("guard"))]
            )
            .unwrap(),
            bin
        );
    }

    #[test]
    fn incomplete_release_transaction_restores_old_binary_and_overlay() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        std::fs::create_dir(&install).unwrap();
        let binary = install.join("omgb");
        let plugin = install.join("plugin-file");
        std::fs::write(&binary, "new binary").unwrap();
        std::fs::write(&plugin, "new plugin").unwrap();
        let transaction = install.join(RELEASE_TRANSACTION_DIR);
        std::fs::create_dir(&transaction).unwrap();
        std::fs::create_dir(transaction.join("backup")).unwrap();
        std::fs::write(transaction.join("backup/binary"), "old binary").unwrap();
        std::fs::write(transaction.join("backup/0"), "old plugin").unwrap();
        let journal = ReleaseJournal {
            state: "applying".into(),
            binary_backup: "backup/binary".into(),
            entries: vec![ReleaseJournalEntry {
                destination: "plugin-file".into(),
                backup: Some("backup/0".into()),
                staged: "new/0".into(),
            }],
        };
        std::fs::write(
            transaction.join("journal.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();

        recover_incomplete_release_transaction(&install, &binary).unwrap();
        assert_eq!(std::fs::read_to_string(&binary).unwrap(), "old binary");
        assert_eq!(std::fs::read_to_string(&plugin).unwrap(), "old plugin");
        assert!(!transaction.exists());
    }

    #[test]
    fn recovery_rejects_journal_paths_outside_the_installation() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        std::fs::create_dir(&install).unwrap();
        let binary = install.join("omgb");
        std::fs::write(&binary, "binary").unwrap();
        let transaction = install.join(RELEASE_TRANSACTION_DIR);
        std::fs::create_dir(&transaction).unwrap();
        std::fs::create_dir(transaction.join("backup")).unwrap();
        std::fs::write(transaction.join("backup/binary"), "old binary").unwrap();
        let journal = ReleaseJournal {
            state: "applying".into(),
            binary_backup: "backup/binary".into(),
            entries: vec![ReleaseJournalEntry {
                destination: "../outside".into(),
                backup: None,
                staged: "new/0".into(),
            }],
        };
        std::fs::write(
            transaction.join("journal.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();

        assert!(recover_incomplete_release_transaction(&install, &binary).is_err());
    }

    #[test]
    fn recovery_rejects_unsafe_staged_paths_without_touching_the_installation() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        std::fs::create_dir(&install).unwrap();
        let binary = install.join("omgb");
        let plugin = install.join("plugin-file");
        std::fs::write(&binary, "current binary").unwrap();
        std::fs::write(&plugin, "current plugin").unwrap();
        let transaction = install.join(RELEASE_TRANSACTION_DIR);
        std::fs::create_dir(&transaction).unwrap();
        std::fs::create_dir(transaction.join("backup")).unwrap();
        std::fs::write(transaction.join("backup/binary"), "old binary").unwrap();
        std::fs::write(transaction.join("backup/0"), "old plugin").unwrap();
        let journal = ReleaseJournal {
            state: "applying".into(),
            binary_backup: "backup/binary".into(),
            entries: vec![ReleaseJournalEntry {
                destination: "plugin-file".into(),
                backup: Some("backup/0".into()),
                staged: "../outside".into(),
            }],
        };
        std::fs::write(
            transaction.join("journal.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();

        assert!(recover_incomplete_release_transaction(&install, &binary).is_err());
        assert_eq!(std::fs::read_to_string(binary).unwrap(), "current binary");
        assert_eq!(std::fs::read_to_string(plugin).unwrap(), "current plugin");
        assert!(transaction.exists());
    }

    #[test]
    fn plugin_overlay_preflight_rejects_conflicting_destination_directory() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("release-plugin");
        let destination = temp.path().join("installed-plugin");
        std::fs::create_dir_all(source.join("commands")).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join("commands/live.md"), "new live command").unwrap();
        std::fs::write(destination.join("commands"), "not a directory").unwrap();

        assert!(validate_release_tree(&source, &destination).is_err());
    }

    #[test]
    fn release_binary_must_be_regular_file() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("omgb");
        std::fs::create_dir(&directory).unwrap();

        assert!(require_regular_file(&directory, "release binary").is_err());
    }

    #[test]
    fn plugin_root_must_have_a_bin_directory() {
        let temp = tempfile::tempdir().unwrap();
        assert!(find_plugin_root(temp.path()).is_none());
        let expected = temp.path().join("plugin");
        std::fs::create_dir_all(expected.join("bin")).unwrap();
        assert_eq!(
            find_plugin_root(temp.path()).as_deref(),
            Some(expected.as_path())
        );
    }
}
