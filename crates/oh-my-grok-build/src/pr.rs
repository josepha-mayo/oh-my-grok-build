//! GitHub PR helper for `omgb`.
//!
//! Wraps `gh` when available for status checks, creating PRs, drafting, and
//! merge-queue inspection. Falls back to helpful diagnostics when `gh` is not
//! installed.

use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::args::{PrCommand, PrCreateArgs, PrMergeArgs, PrReviewArgs, PrStatusArgs, PrUpdateArgs};

pub async fn run_pr(cmd: PrCommand) -> Result<()> {
    match cmd {
        PrCommand::Status(args) => run_status(&args).await,
        PrCommand::Create(args) => run_create(&args, args.draft).await,
        PrCommand::CreateDraft(args) => run_create(&args, true).await,
        PrCommand::Update(args) => run_update(&args).await,
        PrCommand::Merge(args) => run_merge(&args).await,
        PrCommand::Review(args) => run_review(&args).await,
        PrCommand::MergeQueue(args) => run_merge_queue(&args).await,
        PrCommand::Checks(args) => run_checks(&args).await,
    }
}

async fn run_update(args: &PrUpdateArgs) -> Result<()> {
    let branch = resolve_branch(&args.branch)?;
    pr_update(&branch, &args.title, &args.body).await
}

async fn run_merge(args: &PrMergeArgs) -> Result<()> {
    let branch = resolve_branch(&args.branch)?;
    pr_merge(&branch, &args.method).await
}

async fn run_review(args: &PrReviewArgs) -> Result<()> {
    let branch = resolve_branch(&args.branch)?;
    pr_review_request(&branch, &args.reviewers).await
}

fn ensure_gh() -> Result<std::path::PathBuf> {
    which::which("gh").with_context(|| "`gh` CLI not found; install from https://cli.github.com")
}

async fn run_status(args: &PrStatusArgs) -> Result<()> {
    let branch = resolve_branch(&args.branch)?;
    println!("{}", pr_summarize(&branch).await?);
    Ok(())
}

async fn run_create(args: &PrCreateArgs, draft: bool) -> Result<()> {
    ensure_gh()?;
    let mut cmd = tokio::process::Command::new("gh");
    cmd.arg("pr")
        .arg("create")
        .arg("--title")
        .arg(&args.title)
        .arg("--body")
        .arg(&args.body);
    if draft {
        cmd.arg("--draft");
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_gh(cmd).await?;
    Ok(())
}

async fn run_checks(args: &PrStatusArgs) -> Result<()> {
    let branch = resolve_branch(&args.branch)?;
    let failures = pr_check_failures(&branch).await?;
    if failures.is_empty() {
        println!("No check failures for {branch}.");
        return Ok(());
    }
    for name in &failures {
        println!("✗ {name}");
    }
    bail!("{} check(s) failed", failures.len());
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

fn validate_branch_name(branch: &str) -> Result<()> {
    if branch.is_empty() {
        bail!("branch name must not be empty");
    }
    if branch.starts_with('-') {
        bail!("branch name must not start with '-'");
    }
    if branch.starts_with('/') || branch.ends_with('/') {
        bail!("branch name must not start or end with '/'");
    }
    if branch.contains("//") || branch.contains("..") {
        bail!("branch name must not contain '//' or '..' components");
    }
    if branch
        .chars()
        .any(|c| !(c.is_alphanumeric() || matches!(c, '.' | '_' | '/' | '-')))
    {
        bail!("branch name contains disallowed character");
    }
    Ok(())
}

fn resolve_branch(branch: &Option<String>) -> Result<String> {
    let branch = branch.clone().unwrap_or_else(|| {
        std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default()
    });
    if branch.is_empty() {
        bail!("could not determine current git branch; pass --branch");
    }
    validate_branch_name(&branch)?;
    Ok(branch)
}

#[derive(Debug, Deserialize)]
struct GhPrView {
    state: Option<String>,
    url: Option<String>,
    number: Option<u64>,
    title: Option<String>,
    is_draft: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GhCheck {
    name: String,
    state: String,
    #[allow(dead_code)]
    link: String,
}

#[allow(dead_code)]
struct PrData {
    state: String,
    url: String,
    number: u64,
    title: String,
    is_in_merge_queue: bool,
}

fn normalize_state(state: Option<&str>, is_draft: bool) -> String {
    match state.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("merged") => "merged".into(),
        Some("closed") => "closed".into(),
        _ if is_draft => "draft".into(),
        _ => "open".into(),
    }
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
    let state = normalize_state(parsed.state.as_deref(), parsed.is_draft.unwrap_or(false));
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

pub async fn pr_status_json(branch: &str) -> Result<Value> {
    validate_branch_name(branch)?;
    ensure_gh()?;
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args([
        "pr",
        "view",
        branch,
        "--json",
        "state,url,number,title,body,headRefName,baseRefName,isDraft",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let out = run_gh(cmd).await?;
    serde_json::from_str(&out)
        .with_context(|| format!("could not parse `gh pr view` JSON for {branch}"))
}

pub async fn pr_update(branch: &str, title: &str, body: &str) -> Result<()> {
    validate_branch_name(branch)?;
    ensure_gh()?;
    let mut cmd = tokio::process::Command::new("gh");
    cmd.arg("pr")
        .arg("edit")
        .arg(branch)
        .arg("--title")
        .arg(title)
        .arg("--body")
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_gh(cmd).await?;
    Ok(())
}

pub async fn pr_merge(branch: &str, method: &str) -> Result<()> {
    validate_branch_name(branch)?;
    ensure_gh()?;
    let method = method.to_ascii_lowercase();
    if !matches!(method.as_str(), "merge" | "squash" | "rebase") {
        bail!("unsupported merge method: {method}; expected merge, squash, or rebase");
    }
    let mut cmd = tokio::process::Command::new("gh");
    cmd.arg("pr")
        .arg("merge")
        .arg(branch)
        .arg(format!("--{method}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_gh(cmd).await?;
    Ok(())
}

pub async fn pr_review_request(branch: &str, reviewers: &[String]) -> Result<()> {
    validate_branch_name(branch)?;
    if reviewers.is_empty() {
        bail!("at least one reviewer is required");
    }
    for r in reviewers {
        if r.is_empty() || r.starts_with('-') {
            bail!("invalid reviewer: {r}");
        }
        if r.chars()
            .any(|c| !(c.is_alphanumeric() || matches!(c, '-' | '/')))
        {
            bail!("reviewer contains invalid character: {r}");
        }
    }
    ensure_gh()?;
    let list = reviewers.join(",");
    let mut cmd = tokio::process::Command::new("gh");
    cmd.arg("pr")
        .arg("edit")
        .arg(branch)
        .arg("--add-reviewer")
        .arg(&list)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_gh(cmd).await?;
    Ok(())
}

pub async fn pr_check_failures(branch: &str) -> Result<Vec<String>> {
    validate_branch_name(branch)?;
    ensure_gh()?;
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(["pr", "checks", branch, "--json", "name,state,link"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = run_gh(cmd).await?;
    let checks: Vec<GhCheck> = serde_json::from_str(&out)
        .with_context(|| format!("could not parse `gh pr checks` output for {branch}"))?;
    Ok(checks
        .into_iter()
        .filter(|c| c.state == "failure")
        .map(|c| c.name)
        .collect())
}

pub async fn pr_summarize(branch: &str) -> Result<String> {
    let json = pr_status_json(branch).await?;
    let title = json["title"].as_str().unwrap_or("?");
    let number = json["number"].as_u64().unwrap_or(0);
    let state = json["state"].as_str().unwrap_or("?");
    let url = json["url"].as_str().unwrap_or("");
    let head = json["headRefName"].as_str().unwrap_or("?");
    let base = json["baseRefName"].as_str().unwrap_or("?");
    let failures = pr_check_failures(branch).await.unwrap_or_default();
    let check_line = if failures.is_empty() {
        "checks passing".into()
    } else {
        format!("{} failing: {}", failures.len(), failures.join(", "))
    };
    Ok(format!(
        "PR #{number} \"{title}\" [{state}] {url}\n{head} -> {base}\n{check_line}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_state_normalization() {
        assert_eq!(normalize_state(Some("MERGED"), false), "merged");
        assert_eq!(normalize_state(Some("closed"), false), "closed");
        assert_eq!(normalize_state(Some("OPEN"), true), "draft");
        assert_eq!(normalize_state(Some("open"), false), "open");
        assert_eq!(normalize_state(None, false), "open");
        assert_eq!(normalize_state(None, true), "draft");
        assert_eq!(normalize_state(Some("merged"), true), "merged");
    }

    #[test]
    fn parse_gh_checks_json() {
        let raw = r#"[
            {"name":"ci","state":"success","link":"https://example.com/ci"},
            {"name":"lint","state":"failure","link":"https://example.com/lint"}
        ]"#;
        let checks: Vec<GhCheck> = serde_json::from_str(raw).unwrap();
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].state, "success");
        assert_eq!(checks[1].state, "failure");
    }

    #[test]
    fn branch_name_validation() {
        assert!(validate_branch_name("feature/foo-123").is_ok());
        assert!(validate_branch_name("").is_err());
        assert!(validate_branch_name("-foo").is_err());
        assert!(validate_branch_name("foo/../bar").is_err());
        assert!(validate_branch_name("foo bar").is_err());
    }
}
