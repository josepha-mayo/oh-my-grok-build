//! Plugin marketplace commands for `omgb`.
//!
//! Installs plugins from git URLs or local directories into `~/.omgb/plugins`
//! after validating a manifest and copying without following symlinks.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use crate::args::{PluginCommand, PluginInstallArgs};

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

/// Expand `~` at the start of a path string and resolve it against the
/// current working directory when relative. Symlinks and paths that escape
/// the current working directory are rejected to prevent local file reads.
fn resolve_local_plugin_source(source: &str) -> Result<PathBuf> {
    // Expand a leading `~` to the home directory, then canonicalize and require
    // the resolved path to be under the current working directory.
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

async fn clone_plugin(url: &str, dest: &Path) -> Result<()> {
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

    let status = tokio::process::Command::new("git")
        .args(["clone", "--depth", "1", url])
        .arg(&tmp)
        .stdin(Stdio::null())
        .env("HOME", &tmp_home)
        .env("USERPROFILE", &tmp_home)
        .env("XDG_CONFIG_HOME", &tmp_home)
        .env("GIT_TEMPLATE_DIR", &tmp_home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .await
        .context("spawn git")?;

    let _ = std::fs::remove_dir_all(&tmp_home);

    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!("git clone failed");
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

pub async fn run_plugin(cmd: PluginCommand) -> Result<()> {
    match cmd {
        PluginCommand::List => list_plugins().await,
        PluginCommand::Install(args) => install_plugin(&args).await,
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
            println!("{} ({}) @ {}", name.to_string_lossy(), kind, path.display());
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
        clone_plugin(&args.source, &dest).await?;
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

    println!("installed plugin {name} to {}", dest.display());
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
            };
            assert!(
                install_plugin(&args).await.is_err(),
                "{source} should be rejected"
            );
        }
    }
}
