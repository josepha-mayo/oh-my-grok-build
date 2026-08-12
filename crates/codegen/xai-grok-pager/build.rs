use std::path::{Path, PathBuf};
use std::process::Command;

fn git_output(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.trim().to_string())
        .filter(|output| !output.is_empty())
}

fn git_path(manifest_dir: &Path, logical_path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(git_output(
        manifest_dir,
        &["rev-parse", "--git-path", logical_path],
    )?);
    Some(if path.is_absolute() {
        path
    } else {
        manifest_dir.join(path)
    })
}

fn track_if_present(path: Option<PathBuf>) {
    if let Some(path) = path.filter(|path| path.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=GROK_VERSION");

    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_else(|| ".".into()));
    track_if_present(git_path(&manifest_dir, "HEAD"));
    if let Some(reference) = git_output(&manifest_dir, &["symbolic-ref", "-q", "HEAD"]) {
        track_if_present(git_path(&manifest_dir, &reference));
    }
    track_if_present(git_path(&manifest_dir, "packed-refs"));

    let commit = git_output(&manifest_dir, &["rev-parse", "--short", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());

    let version = std::env::var("GROK_VERSION")
        .or_else(|_| std::env::var("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| "0.0.0".to_string());

    println!(
        "cargo:rustc-env=VERSION_WITH_COMMIT={} ({})",
        version, commit
    );
}
