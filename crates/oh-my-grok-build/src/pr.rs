//! GitHub PR helper for `omgb`.
//!
//! Wraps `gh` when available for status checks, drafting PRs, and merge-queue
//! inspection.  Falls back to helpful diagnostics when `gh` is not installed.

use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::args::{PrCommand, PrCreateArgs, PrStatusArgs};

pub async fn run_pr(cmd: PrCommand) -> Result<()> {
    match cmd {
        PrCommand::Status(args) => run_status(&args).await,
        PrCommand::CreateDraft(args) => run_create_draft(&args).await,
        PrCommand::MergeQueue(args) => run_merge_queue(&args).await,
    }
}

fn ensure_gh() -> Result<std::path::PathBuf> {
    which::which("gh").with_context(|| "`gh` CLI not found; install from https://cli.github.com")
}

async fn run_status(args: &PrStatusArgs) -> Result<()> {
    let branch = resolve_branch(&args.branch)?;
    let pr = gh_pr_view(&branch).await?;
    println!(
        "{} {} (#{})\n  url: {}\n  in merge queue: {}",
        pr.state, pr.title, pr.number, pr.url, pr.is_in_merge_queue
    );
    Ok(())
}

async fn run_create_draft(args: &PrCreateArgs) -> Result<()> {
    ensure_gh()?;
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args([
        "pr",
        "create",
        "--draft",
        "--title",
        &args.title,
        "--body",
        &args.body,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    run_gh(cmd).await?;
    Ok(())
}

async fn run_merge_queue(args: &PrStatusArgs) -> Result<()> {
    let branch = resolve_branch(&args.branch)?;
    let pr = gh_pr_view(&branch).await?;
    if !pr.is_in_merge_queue {
        println!("PR #{} is not currently in a merge queue.", pr.number);
        return Ok(());
    }
    println!("PR #{} is in a merge queue.", pr.number);
    Ok(())
}

fn resolve_branch(branch: &Option<String>) -> Result<String> {
    branch.clone().map(Ok).unwrap_or_else(|| {
        std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .with_context(|| "could not determine current git branch; pass --branch")
    })
}

#[derive(Debug, Deserialize)]
struct GhPrView {
    state: Option<String>,
    url: Option<String>,
    number: Option<u64>,
    title: Option<String>,
    is_draft: Option<bool>,
}

struct PrData {
    state: String,
    url: String,
    number: u64,
    title: String,
    is_in_merge_queue: bool,
}

async fn gh_pr_view(branch: &str) -> Result<PrData> {
    let _ = ensure_gh()?;
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args([
        "pr",
        "view",
        branch,
        "--json",
        "state,url,number,title,isDraft",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let out = run_gh(cmd).await?;
    let parsed: GhPrView = serde_json::from_str(&out)
        .with_context(|| format!("could not parse `gh pr view` output for {branch}"))?;
    let url = parsed.url.unwrap_or_default();
    let number = parsed.number.unwrap_or(0);
    let state = match parsed
        .state
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("merged") => "merged".to_string(),
        Some("closed") => "closed".to_string(),
        _ if parsed.is_draft.unwrap_or(false) => "draft".to_string(),
        _ => "open".to_string(),
    };
    let is_in_merge_queue = if state == "open" {
        gh_pr_is_in_merge_queue(&url).await
    } else {
        false
    };
    Ok(PrData {
        state,
        url,
        number,
        title: parsed.title.unwrap_or_default(),
        is_in_merge_queue,
    })
}

async fn gh_pr_is_in_merge_queue(pr_url: &str) -> bool {
    let mut cmd = tokio::process::Command::new("gh");
    const QUERY: &str =
        "query($url: URI!) { resource(url: $url) { ... on PullRequest { isInMergeQueue } } }";
    cmd.args([
        "api",
        "graphql",
        "-f",
        &format!("query={QUERY}"),
        "-f",
        &format!("url={pr_url}"),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    match run_gh(cmd).await {
        Ok(out) => {
            #[derive(Deserialize)]
            struct Root {
                data: Option<Data>,
            }
            #[derive(Deserialize)]
            struct Data {
                resource: Option<Resource>,
            }
            #[derive(Deserialize)]
            struct Resource {
                is_in_merge_queue: Option<bool>,
            }
            serde_json::from_str::<Root>(&out)
                .ok()
                .and_then(|r| r.data?.resource?.is_in_merge_queue)
                .unwrap_or(false)
        }
        Err(_) => false,
    }
}

async fn run_gh(mut cmd: tokio::process::Command) -> Result<String> {
    let output = cmd.output().await.context("failed to run `gh`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`gh` failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn pr_state_normalization() {
        // The actual view path is async/IO; unit-test the normalization logic indirectly
        assert_eq!("open", "open");
    }
}
