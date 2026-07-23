//! Plugin marketplace commands for `omgb`.
//!
//! Installs plugins from git URLs or local directories into `~/.omgb/plugins`
//! after validating a manifest, stripping symlinks, and (for git sources)
//! recording the source so it can be refreshed later. Remote clones are guarded
//! by a hang timeout and optional SHA pinning.

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncReadExt;

use crate::args::{PluginCommand, PluginInstallArgs, PluginRefreshArgs};

const CLONE_TIMEOUT: Duration = Duration::from_secs(120);
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

fn plugin_dir() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("plugins"))
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("plugin name must not be empty");
    }
    if name == "." || name == ".." {
        bail!("plugin name cannot be '.' or '..'");
    }
    if name.contains(['/', '\\', ':', '\0']) {
        bail!("plugin name contains invalid characters: {name}");
    }
    Ok(())
}

fn infer_name(source: &str) -> String {
    let s = source.trim_end_matches('/').trim_end_matches(".git");
    s.rsplit('/').next().unwrap_or(s).to_string()
}

fn has_manifest(dir: &Path) -> bool {
    dir.join("plugin.json").is_file() || dir.join("omgb.json").is_file()
}

fn validate_plugin_dir(dir: &Path) -> Result<()> {
    if !has_manifest(dir) {
        bail!(
            "plugin at {} is missing plugin.json or omgb.json",
            dir.display()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SourceMeta {
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha: Option<String>,
}

fn source_meta_path(dir: &Path) -> PathBuf {
    dir.join(".omgb-source.json")
}

fn read_source_meta(dir: &Path) -> Result<Option<SourceMeta>> {
    let path = source_meta_path(dir);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("{} is not valid JSON", path.display()))
}

fn write_source_meta(dir: &Path, meta: &SourceMeta) -> Result<()> {
    let path = source_meta_path(dir);
    let raw = serde_json::to_string_pretty(meta)?;
    std::fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))
}

/// Expand `~` at the start of a path string and resolve it against the
/// current working directory when relative. Symlinks and paths that escape
/// the current working directory are rejected to prevent local file reads.
fn resolve_local_plugin_source(source: &str) -> Result<PathBuf> {
    let expanded = if source.starts_with("~/") {
        dirs::home_dir()
            .map(|h| h.join(&source[2..]))
            .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?
    } else if source == "~" {
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?
    } else {
        PathBuf::from(source)
    };

    let cwd = std::env::current_dir().context("failed to get current directory")?;
    let src = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };

    let meta = std::fs::symlink_metadata(&src)
        .with_context(|| format!("plugin source does not exist: {}", src.display()))?;
    if meta.is_symlink() {
        bail!("plugin source must not be a symlink");
    }
    if !meta.is_dir() {
        bail!("plugin source is not a directory");
    }

    let canonical_src = dunce::canonicalize(&src)
        .with_context(|| format!("failed to canonicalize {}", src.display()))?;
    let canonical_cwd =
        dunce::canonicalize(&cwd).context("failed to canonicalize current directory")?;

    if canonical_src == canonical_cwd {
        bail!("plugin source cannot be the current working directory");
    }
    if !canonical_src.starts_with(&canonical_cwd) {
        bail!(
            "plugin source must be under the current working directory: {}",
            canonical_cwd.display()
        );
    }

    Ok(canonical_src)
}

/// Recursively copy `src` to `dst`, skipping symlinks and special files to
/// avoid directory traversal and arbitrary file reads.
fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)
        .with_context(|| format!("metadata for {}", src.display()))?;
    if meta.is_symlink() {
        bail!("source directory must not be a symlink");
    }
    if !meta.is_dir() {
        bail!("source must be a directory");
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest = dst.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&path)
            .with_context(|| format!("metadata for {}", path.display()))?;
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_dir(&path, &dest)?;
        } else if meta.is_file() {
            std::fs::copy(&path, &dest)
                .with_context(|| format!("copy {} to {}", path.display(), dest.display()))?;
        }
    }
    Ok(())
}

/// Recursively remove any symlink (or junction) entries in `dir` so a cloned
/// plugin cannot carry absolute or `..` symlinks that escape the install tree.
fn remove_symlinks_in_dir(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .with_context(|| format!("metadata for {}", path.display()))?;
        if meta.is_symlink() {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove symlink {}", path.display()))?;
        } else if meta.is_dir() {
            remove_symlinks_in_dir(&path)?;
        }
    }
    Ok(())
}

fn base_git_cmd(tmp_home: &Path) -> tokio::process::Command {
    let mut cmd = crate::git_cmd();
    cmd.stdin(Stdio::null())
        .env("HOME", tmp_home)
        .env("USERPROFILE", tmp_home)
        .env("XDG_CONFIG_HOME", tmp_home)
        .env("GIT_TEMPLATE_DIR", tmp_home)
        .env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

/// Run a git command with a hard timeout. stdout/stderr are captured so we can
/// surface meaningful errors without blocking on pipe back-pressure.
async fn run_git(tmp_home: &Path, args: &[&str], timeout: Duration) -> Result<Output> {
    let mut child = base_git_cmd(tmp_home)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn git")?;

    let mut stdout = child.stdout.take().context("no stdout")?;
    let mut stderr = child.stderr.take().context("no stderr")?;
    let out_handle = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s).await;
        s
    });
    let err_handle = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            let (out, err) = tokio::join!(out_handle, err_handle);
            Ok(Output {
                status,
                stdout: out.unwrap_or_default().into_bytes(),
                stderr: err.unwrap_or_default().into_bytes(),
            })
        }
        Ok(Err(e)) => {
            out_handle.abort();
            err_handle.abort();
            bail!("git command failed: {e}")
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            out_handle.abort();
            err_handle.abort();
            bail!("git command timed out after {}s", timeout.as_secs());
        }
    }
}

/// Clone `url` into `dest` (a temporary directory), optionally pinned to a SHA.
/// The clone is guarded by `CLONE_TIMEOUT` to contain hung git sources.
async fn clone_plugin(url: &str, dest: &Path, require_sha: Option<&str>) -> Result<()> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("only http(s) git URLs are supported for remote plugin installation");
    }
    crate::net::validate_url(url, false, false).await?;

    let parent = dest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("destination has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".clone-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)?;

    let tmp_home = std::env::temp_dir().join(format!("omgb-git-home-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_home)?;

    let result = if let Some(sha) = require_sha {
        clone_with_sha(url, &tmp, &tmp_home, sha).await
    } else {
        clone_shallow(url, &tmp, &tmp_home).await
    };

    let _ = std::fs::remove_dir_all(&tmp_home);

    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }

    remove_symlinks_in_dir(&tmp).with_context(|| "failed to scrub symlinks from cloned plugin")?;

    validate_plugin_dir(&tmp)?;

    if let Err(e) = std::fs::remove_dir_all(tmp.join(".git")) {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!("failed to strip .git directory: {e}");
    }

    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!("failed to move cloned plugin into place: {e}");
    }
    Ok(())
}

async fn clone_shallow(url: &str, tmp: &Path, tmp_home: &Path) -> Result<()> {
    let tmp_str = tmp.to_string_lossy();
    let output = run_git(
        tmp_home,
        &["clone", "--depth", "1", url, &tmp_str],
        CLONE_TIMEOUT,
    )
    .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git clone failed: {stderr}");
    }
    Ok(())
}

fn validate_sha(sha: &str) -> Result<()> {
    let trimmed = sha.trim();
    if trimmed.is_empty() {
        bail!("pinned SHA must not be empty");
    }
    if trimmed.len() < 7 {
        bail!("pinned SHA must be at least 7 characters");
    }
    if trimmed.len() > 64 {
        bail!("pinned SHA must be at most 64 characters");
    }
    if !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("pinned SHA must be a hexadecimal string");
    }
    Ok(())
}

async fn clone_with_sha(url: &str, tmp: &Path, tmp_home: &Path, sha: &str) -> Result<()> {
    validate_sha(sha)?;
    let want = sha.trim().to_lowercase();
    let tmp_str = tmp.to_string_lossy();

    // Do a full clone so we can check out an arbitrary SHA. Plugins are small,
    // and this avoids servers that do not support reachability SHA fetches.
    let clone = run_git(tmp_home, &["clone", url, &tmp_str], CLONE_TIMEOUT).await?;
    if !clone.status.success() {
        let stderr = String::from_utf8_lossy(&clone.stderr);
        bail!("git clone failed: {stderr}");
    }

    let checkout = run_git(
        tmp_home,
        &["-C", &tmp_str, "checkout", sha],
        GIT_COMMAND_TIMEOUT,
    )
    .await?;
    if !checkout.status.success() {
        let stderr = String::from_utf8_lossy(&checkout.stderr);
        bail!("git checkout failed: {stderr}");
    }

    let actual = run_git(
        tmp_home,
        &["-C", &tmp_str, "rev-parse", "HEAD"],
        GIT_COMMAND_TIMEOUT,
    )
    .await?;
    if !actual.status.success() {
        bail!("git rev-parse failed");
    }
    let actual_sha = String::from_utf8_lossy(&actual.stdout)
        .trim()
        .to_lowercase();
    if actual_sha != want && !actual_sha.starts_with(&want) {
        bail!("checked out SHA {actual_sha} does not match required {sha}");
    }
    Ok(())
}

pub async fn run_plugin(cmd: PluginCommand) -> Result<()> {
    match cmd {
        PluginCommand::List => list_plugins().await,
        PluginCommand::Install(args) => install_plugin(&args).await,
        PluginCommand::Remove { name } => remove_plugin(&name).await,
        PluginCommand::Refresh(args) => refresh_plugins(&args).await,
    }
}

async fn list_plugins() -> Result<()> {
    let dirs = [plugin_dir()?, PathBuf::from("plugin")];
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            let kind = if path.join("omgb.json").is_file() {
                "omgb"
            } else {
                "plugin"
            };
            let meta = read_source_meta(&path).ok().flatten();
            let pin = meta
                .and_then(|m| m.sha)
                .map(|s| format!(" @ {s:.12}"))
                .unwrap_or_default();
            println!(
                "{} ({}) @ {}{pin}",
                name.to_string_lossy(),
                kind,
                path.display()
            );
        }
    }
    Ok(())
}

async fn install_plugin(args: &PluginInstallArgs) -> Result<()> {
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| infer_name(&args.source));
    validate_name(&name)?;

    let dest = plugin_dir()?.join(&name);
    if dest.exists() {
        bail!("plugin {name} already installed; remove it first");
    }

    let parent = dest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("destination has no parent"))?;
    std::fs::create_dir_all(parent)?;

    if args.source.starts_with("http://") || args.source.starts_with("https://") {
        clone_plugin(&args.source, &dest, args.require_sha.as_deref()).await?;
    } else if args.source.starts_with("ssh://")
        || args.source.starts_with("git://")
        || args.source.starts_with("git@")
        || args.source.starts_with("file://")
    {
        bail!(
            "unsupported git URL scheme; use https or a local directory under the current working directory"
        );
    } else {
        let src = resolve_local_plugin_source(&args.source)?;
        let tmp = parent.join(format!(".install-{}", uuid::Uuid::new_v4()));
        copy_dir(&src, &tmp)?;
        validate_plugin_dir(&tmp)?;
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e.into());
        }
    }

    let meta = SourceMeta {
        source: args.source.clone(),
        sha: args.require_sha.as_ref().map(|s| s.trim().to_lowercase()),
    };
    let _ = write_source_meta(&dest, &meta);

    println!("installed plugin {name} to {}", dest.display());
    Ok(())
}

async fn remove_plugin(name: &str) -> Result<()> {
    validate_name(name)?;
    let dest = plugin_dir()?.join(name);
    if !dest.exists() {
        bail!("plugin '{name}' is not installed");
    }
    std::fs::remove_dir_all(&dest)?;
    println!("removed plugin {name}");
    Ok(())
}

async fn refresh_plugins(args: &PluginRefreshArgs) -> Result<()> {
    let dir = plugin_dir()?;
    let targets: Vec<(String, PathBuf)> = if let Some(name) = &args.name {
        validate_name(name)?;
        let p = dir.join(name);
        if !p.exists() {
            bail!("plugin '{name}' is not installed");
        }
        vec![(name.clone(), p)]
    } else {
        if !dir.exists() {
            return Ok(());
        }
        std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect()
    };

    if targets.is_empty() {
        println!("no installed plugins to refresh");
        return Ok(());
    }

    // Run all refreshes concurrently so no single hung source blocks the rest.
    let futures: Vec<_> = targets
        .into_iter()
        .filter_map(|(name, path)| {
            let meta = match read_source_meta(&path) {
                Ok(Some(m)) => m,
                Ok(None) => {
                    eprintln!("warning: plugin {name} has no source metadata; skipping");
                    return None;
                }
                Err(e) => {
                    eprintln!("warning: plugin {name}: {e}; skipping");
                    return None;
                }
            };
            if !meta.source.starts_with("http://") && !meta.source.starts_with("https://") {
                eprintln!("warning: plugin {name} source is not remote; skipping");
                return None;
            }
            Some(tokio::spawn(async move {
                (
                    name.clone(),
                    refresh_one(&name, &path, &meta.source, meta.sha.as_deref()).await,
                )
            }))
        })
        .collect();
    for handle in futures {
        match handle.await {
            Ok((name, Ok(()))) => println!("refreshed plugin {name}"),
            Ok((_, Err(e))) => eprintln!("refresh failed: {e}"),
            Err(e) => eprintln!("refresh task panicked: {e}"),
        }
    }
    Ok(())
}

async fn refresh_one(name: &str, path: &Path, source: &str, sha: Option<&str>) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".refresh-{name}-{}", uuid::Uuid::new_v4()));
    let backup = parent.join(format!(".backup-{name}-{}", uuid::Uuid::new_v4()));

    clone_plugin(source, &tmp, sha).await?;

    if let Err(e) = std::fs::rename(path, &backup) {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!("failed to backup old plugin {name}: {e}");
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::rename(&backup, path);
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&backup);
        bail!("failed to move refreshed plugin {name}: {e}");
    }
    if let Err(e) = std::fs::remove_dir_all(&backup) {
        eprintln!("warning: failed to remove old plugin backup {name}: {e}");
    }

    let meta = SourceMeta {
        source: source.into(),
        sha: sha.map(|s| s.into()),
    };
    let _ = write_source_meta(path, &meta);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_name_from_url() {
        assert_eq!(infer_name("https://github.com/x/foo.git"), "foo");
        assert_eq!(infer_name("/path/to/bar"), "bar");
    }

    #[test]
    fn copy_dir_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("marketplace-test-{}", uuid::Uuid::new_v4()));
        let src = tmp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("f.txt"), "hi").unwrap();
        let dst = tmp.join("dst");
        copy_dir(&src, &dst).unwrap();
        assert!(dst.join("f.txt").exists());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    #[cfg(unix)]
    fn copy_dir_skips_symlinks() {
        let tmp = std::env::temp_dir().join(format!("marketplace-test-{}", uuid::Uuid::new_v4()));
        let src = tmp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(tmp.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(tmp.join("secret.txt"), src.join("link.txt")).unwrap();
        let dst = tmp.join("dst");
        copy_dir(&src, &dst).unwrap();
        assert!(!dst.join("link.txt").exists());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    #[cfg(unix)]
    fn remove_symlinks_in_dir_scrubs() {
        let tmp = std::env::temp_dir().join(format!("marketplace-test-{}", uuid::Uuid::new_v4()));
        let dir = tmp.join("plugin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), "{}").unwrap();
        std::fs::write(tmp.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(tmp.join("secret.txt"), dir.join("link.txt")).unwrap();
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::os::unix::fs::symlink(tmp.join("secret.txt"), sub.join("nested-link.txt")).unwrap();

        remove_symlinks_in_dir(&dir).unwrap();

        assert!(dir.join("manifest.json").exists());
        assert!(!dir.join("link.txt").exists());
        assert!(sub.exists());
        assert!(!sub.join("nested-link.txt").exists());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn validate_name_rejects_path_traversal() {
        assert!(validate_name("../etc").is_err());
        assert!(validate_name("C:/windows").is_err());
        assert!(validate_name("good-name").is_ok());
    }

    #[test]
    fn validate_plugin_dir_requires_manifest() {
        let tmp = std::env::temp_dir().join(format!("marketplace-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(validate_plugin_dir(&tmp).is_err());
        std::fs::write(tmp.join("plugin.json"), "{}").unwrap();
        assert!(validate_plugin_dir(&tmp).is_ok());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn resolve_local_plugin_source_restricts_paths() {
        let cwd = std::env::current_dir().unwrap();
        let name = format!("marketplace-local-test-{}", uuid::Uuid::new_v4());
        let good = cwd.join(&name);
        std::fs::create_dir_all(&good).unwrap();
        assert!(resolve_local_plugin_source(&name).is_ok());
        std::fs::remove_dir_all(&good).ok();

        assert!(resolve_local_plugin_source(".").is_err());

        let outside =
            std::env::temp_dir().join(format!("marketplace-outside-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();
        assert!(resolve_local_plugin_source(outside.to_str().unwrap()).is_err());
        std::fs::remove_dir_all(&outside).ok();

        #[cfg(unix)]
        {
            let target = cwd.join(format!(
                "marketplace-symlink-target-{}",
                uuid::Uuid::new_v4()
            ));
            let link = cwd.join(format!("marketplace-symlink-link-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&target).unwrap();
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(resolve_local_plugin_source(link.to_str().unwrap()).is_err());
            std::fs::remove_dir_all(&target).ok();
            let _ = std::fs::remove_file(&link);
        }
    }

    #[tokio::test]
    async fn install_plugin_rejects_unsupported_schemes() {
        for source in &[
            "ssh://example.com/repo.git",
            "git://example.com/repo.git",
            "git@example.com:repo.git",
            "file:///etc/passwd",
        ] {
            let args = PluginInstallArgs {
                source: source.to_string(),
                name: Some("test".into()),
                require_sha: None,
            };
            assert!(
                install_plugin(&args).await.is_err(),
                "{source} should be rejected"
            );
        }
    }
}
