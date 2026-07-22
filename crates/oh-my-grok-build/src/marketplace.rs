//! Plugin marketplace commands for `omgb`.
//!
//! Lists locally installed plugins and installs plugins from git URLs or local
//! directories into `~/.omgb/plugins`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::args::{PluginCommand, PluginInstallArgs};

fn plugin_dir() -> PathBuf {
    crate::memory::omgb_home().join("plugins")
}

pub async fn run_plugin(cmd: PluginCommand) -> Result<()> {
    match cmd {
        PluginCommand::List => list_plugins().await,
        PluginCommand::Install(args) => install_plugin(&args).await,
    }
}

async fn list_plugins() -> Result<()> {
    let dirs = [plugin_dir(), PathBuf::from("plugin")];
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
            let meta = entry.path().join("omgb.json");
            let kind = if meta.exists() { "omgb" } else { "plugin" };
            println!(
                "{} ({}) @ {}",
                name.to_string_lossy(),
                kind,
                entry.path().display()
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
    if name.is_empty() {
        bail!("could not infer plugin name from source");
    }
    let dest = plugin_dir().join(&name);
    if dest.exists() {
        bail!("plugin {name} already installed; remove it first");
    }
    std::fs::create_dir_all(plugin_dir())?;
    if is_git_url(&args.source) {
        clone_plugin(&args.source, &dest).await?;
    } else {
        let src = PathBuf::from(&args.source);
        if !src.exists() || !src.is_dir() {
            bail!("plugin source is not a directory or git URL");
        }
        copy_dir(&src, &dest)?;
    }
    println!("installed plugin {name} to {}", dest.display());
    Ok(())
}

fn is_git_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("git@")
        || s.starts_with("ssh://")
        || s.starts_with("git://")
        || s.ends_with(".git")
}

fn infer_name(source: &str) -> String {
    let s = source.trim_end_matches('/').trim_end_matches(".git");
    s.rsplit('/').next().unwrap_or(s).to_string()
}

async fn clone_plugin(url: &str, dest: &Path) -> Result<()> {
    let status = tokio::process::Command::new("git")
        .args(["clone", "--depth", "1", url, &dest.to_string_lossy()])
        .stdin(std::process::Stdio::null())
        .status()
        .await
        .context("spawn git")?;
    if !status.success() {
        bail!("git clone failed");
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &dest)?;
        } else {
            std::fs::copy(&path, &dest)?;
        }
    }
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
}
