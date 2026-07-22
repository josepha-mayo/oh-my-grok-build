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

fn is_git_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("git@")
        || s.starts_with("ssh://")
        || s.starts_with("git://")
        || s.ends_with(".git")
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

/// Recursively copy `src` to `dst`, skipping symlinks and special files to
/// avoid directory traversal and arbitrary file reads.
fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
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

async fn clone_plugin(url: &str, dest: &Path) -> Result<()> {
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
        .status()
        .await
        .context("spawn git")?;

    let _ = std::fs::remove_dir_all(&tmp_home);

    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!("git clone failed");
    }

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

    if is_git_url(&args.source) {
        clone_plugin(&args.source, &dest).await?;
    } else {
        let src = PathBuf::from(&args.source);
        if !src.exists() || !src.is_dir() {
            bail!("plugin source is not a directory or git URL");
        }
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
}
