//! Deep arXiv/web research for `omgb`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use quick_xml::Reader;
use quick_xml::events::Event;
use scraper::{Html, Selector};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::net::{http_get_text, is_non_public_ip, validate_url};

fn safe_filename(input: &str) -> String {
    let mut out = String::new();
    let mut prev_replaced = false;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
            out.push(c);
            prev_replaced = false;
        } else if !prev_replaced {
            out.push('_');
            prev_replaced = true;
        }
    }
    let separators = ['.', '_', '-'];
    out = out.trim_end_matches(&separators[..]).to_string();
    out = out.trim_start_matches(&separators[..]).to_string();
    if out.is_empty() {
        out.push_str("report");
    }
    out
}

fn sanitize_output_path(dir: &Path, raw: &Path) -> Result<PathBuf> {
    let mut has_normal = false;
    for comp in raw.components() {
        match comp {
            std::path::Component::Normal(_) => has_normal = true,
            std::path::Component::CurDir => {}
            _ => bail!("invalid output path: must be a relative path with no '..' components"),
        }
    }
    if !has_normal {
        bail!("invalid output path: must contain at least one file or directory component");
    }
    Ok(dir.join(raw))
}

fn prepare_output_path(root: &Path, path: &Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
    crate::providers::restrict_omg_directory_permissions(root)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("research output path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let canonical_root = dunce::canonicalize(root)
        .with_context(|| format!("canonicalize research root {}", root.display()))?;
    let canonical_parent = dunce::canonicalize(parent)
        .with_context(|| format!("canonicalize research output parent {}", parent.display()))?;
    if !canonical_parent.starts_with(&canonical_root) {
        bail!(
            "research output parent escapes {}: {}",
            canonical_root.display(),
            canonical_parent.display()
        );
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => bail!("research output must be a regular file: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect output {}", path.display())),
    }
}

#[derive(Debug)]
struct ArxivEntry {
    title: String,
    summary: String,
    id: String,
    pdf: String,
    authors: Vec<String>,
}

#[derive(Debug, Default)]
struct WebResult {
    title: String,
    url: String,
    snippet: String,
}

const SEARCH_USER_AGENT: &str = concat!(
    "oh-my-grok-build/",
    env!("CARGO_PKG_VERSION"),
    " (research; +https://oh-my-grok.build)"
);
const DEFAULT_SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const URL_VALIDATE_TIMEOUT: Duration = Duration::from_secs(5);
const PATCH_PROMPT_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_PROMPT_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RESEARCH_REPORT_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESULTS: usize = 100;
const RESEARCH_MANIFEST_VERSION: u8 = 1;
const PATCH_TOOL_POLICY: &str = "grep,list_dir,read_file,web_fetch,web_search";

#[derive(Debug, Serialize)]
struct ResearchRepositoryProvenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    head_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tracked_dirty: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ResearchBudgetManifest {
    max_results: usize,
    search_timeout_seconds: u64,
    url_validation_timeout_seconds: u64,
    report_bytes_limit: usize,
    patch_timeout_seconds: u64,
    patch_output_bytes_limit: u64,
}

#[derive(Debug, Serialize)]
struct ResearchArtifactManifest {
    kind: &'static str,
    relative_path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct ResearchSourceManifest {
    name: &'static str,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_sha256: Option<String>,
}

#[derive(Debug, Serialize)]
struct PatchGenerationManifest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_execution_fingerprint: Option<String>,
    prompt_sha256: String,
    tool_policy_sha256: String,
    yolo: bool,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_sha256: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResearchRunManifest {
    schema_version: u8,
    run_id: String,
    created_at: String,
    topic_sha256: String,
    topic_bytes: usize,
    requested_count: usize,
    effective_count: usize,
    sources: Vec<ResearchSourceManifest>,
    repository: ResearchRepositoryProvenance,
    budget: ResearchBudgetManifest,
    artifacts: Vec<ResearchArtifactManifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    patch_generation: Option<PatchGenerationManifest>,
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect research artifact {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!(
            "research artifact is not a regular file: {}",
            path.display()
        );
    }
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("open research artifact {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read research artifact {}", path.display()))?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(read as u64);
        digest.update(&buffer[..read]);
    }
    Ok((format!("{:x}", digest.finalize()), bytes))
}

fn artifact_manifest(
    root: &Path,
    path: &Path,
    kind: &'static str,
    expected_sha256: &str,
) -> Result<ResearchArtifactManifest> {
    let relative = path.strip_prefix(root).with_context(|| {
        format!(
            "research artifact {} is outside {}",
            path.display(),
            root.display()
        )
    })?;
    let (actual_sha256, bytes) = hash_file(path)?;
    if actual_sha256 != expected_sha256 {
        bail!(
            "research artifact changed before it could be recorded: {}",
            path.display()
        );
    }
    Ok(ResearchArtifactManifest {
        kind,
        relative_path: relative.to_string_lossy().replace('\\', "/"),
        sha256: actual_sha256,
        bytes,
    })
}

fn repository_provenance() -> ResearchRepositoryProvenance {
    fn git(args: &[&str]) -> Option<std::process::Output> {
        std::process::Command::new("git")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .ok()
    }

    let head_commit = git(&["rev-parse", "--verify", "HEAD"])
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let tracked_dirty = head_commit.as_ref().and_then(|_| {
        let changed = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok()
                .map(|status| !status.success())
        };
        let worktree_changed = changed(&["diff", "--quiet"])?;
        let index_changed = changed(&["diff", "--cached", "--quiet"])?;
        Some(worktree_changed || index_changed)
    });
    ResearchRepositoryProvenance {
        head_commit,
        tracked_dirty,
    }
}

fn write_run_manifest(root: &Path, manifest: &ResearchRunManifest) -> Result<(PathBuf, String)> {
    let runs = root.join("runs");
    std::fs::create_dir_all(&runs)?;
    crate::providers::restrict_omg_directory_permissions(&runs)?;
    let path = runs.join(format!("{}.json", manifest.run_id));
    if path.exists() {
        bail!("research run manifest already exists: {}", path.display());
    }
    let bytes = serde_json::to_vec_pretty(manifest)?;
    let manifest_sha256 = sha256(&bytes);
    crate::providers::write_file_atomic(&path, &String::from_utf8(bytes)?, true)?;
    let (actual_sha256, _) = hash_file(&path)?;
    if actual_sha256 != manifest_sha256 {
        bail!("research run manifest failed post-write verification");
    }
    Ok((path, manifest_sha256))
}

struct ResearchOutput {
    report: String,
    sources: Vec<ResearchSourceManifest>,
}

async fn research_with_provenance(topic: &str, count: usize) -> Result<ResearchOutput> {
    let count = count.min(MAX_RESULTS);
    let mut report = format!("Research: {}\n\n", topic);
    let mut found = false;
    let mut sources = Vec::with_capacity(2);

    match arxiv_research(topic, count).await {
        Ok(text) => {
            report.push_str(&text);
            found = true;
            sources.push(ResearchSourceManifest {
                name: "arxiv_atom_v1",
                outcome: "succeeded",
                error_sha256: None,
            });
        }
        Err(error) => {
            report.push_str(&format!("arXiv search unavailable: {error}\n\n"));
            sources.push(ResearchSourceManifest {
                name: "arxiv_atom_v1",
                outcome: "failed",
                error_sha256: Some(sha256(error.to_string().as_bytes())),
            });
        }
    }

    match web_search(topic, count).await {
        Ok(text) => {
            if !text.is_empty() {
                report.push_str(&format!("\nWeb results:\n\n{text}"));
                found = true;
            }
            sources.push(ResearchSourceManifest {
                name: "duckduckgo_instant_or_html_v1",
                outcome: if text.is_empty() {
                    "empty"
                } else {
                    "succeeded"
                },
                error_sha256: None,
            });
        }
        Err(error) => {
            report.push_str(&format!("\nWeb search unavailable: {error}\n"));
            sources.push(ResearchSourceManifest {
                name: "duckduckgo_instant_or_html_v1",
                outcome: "failed",
                error_sha256: Some(sha256(error.to_string().as_bytes())),
            });
        }
    }

    if !found {
        bail!("no research results for '{topic}'");
    }
    Ok(ResearchOutput { report, sources })
}

async fn arxiv_research(topic: &str, count: usize) -> Result<String> {
    let query = urlencoding::encode(topic);
    let url = format!(
        "https://export.arxiv.org/api/query?search_query=all:{query}&start=0&max_results={count}&sortBy=relevance&sortOrder=descending"
    );
    let vurl = validate_url(&url, false, false).await?;
    let text = http_get_text(&vurl, None, DEFAULT_SEARCH_TIMEOUT).await?;
    let entries = parse_atom(&text)?;

    if entries.is_empty() {
        bail!("no arXiv results for '{topic}'");
    }

    let mut report = String::from("arXiv results:\n\n");
    for (i, entry) in entries.iter().take(count).enumerate() {
        report.push_str(&format!(
            "{}. {}\n   Authors: {}\n   PDF: {}\n   Summary: {}\n\n",
            i + 1,
            entry.title,
            entry.authors.join(", "),
            entry.pdf,
            entry.summary.replace('\n', " ")
        ));
    }
    Ok(report)
}

async fn ddg_instant_answer(topic: &str, count: usize) -> Option<Vec<WebResult>> {
    let query = urlencoding::encode(topic);
    let url =
        format!("https://api.duckduckgo.com/?q={query}&format=json&no_html=1&skip_disambig=1");
    let vurl = validate_url(&url, false, false).await.ok()?;
    let text = http_get_text(&vurl, None, DEFAULT_SEARCH_TIMEOUT)
        .await
        .ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;

    let mut candidates = Vec::new();
    if let (Some(abstract_text), Some(url)) = (
        json.get("AbstractText")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty()),
        json.get("AbstractURL").and_then(|v| v.as_str()),
    ) {
        candidates.push((
            json.get("Heading")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            url.to_string(),
            abstract_text.to_string(),
        ));
    }

    fn collect_topics(value: &serde_json::Value, out: &mut Vec<(String, String, String)>) {
        if let Some(arr) = value.as_array() {
            for item in arr {
                if let Some(topics) = item.get("Topics") {
                    collect_topics(topics, out);
                } else if let (Some(text), Some(url)) = (
                    item.get("Text").and_then(|v| v.as_str()),
                    item.get("FirstURL").and_then(|v| v.as_str()),
                ) {
                    out.push((String::new(), url.to_string(), text.to_string()));
                }
            }
        }
    }
    if let Some(topics) = json.get("RelatedTopics") {
        collect_topics(topics, &mut candidates);
    }

    let mut out = Vec::new();
    for (title, url, snippet) in candidates {
        if let Some(vurl) = validated_search_url(&url).await {
            out.push(WebResult {
                title,
                url: vurl,
                snippet,
            });
        }
    }

    if out.is_empty() {
        return None;
    }
    out.truncate(count);
    Some(out)
}

async fn web_search_html(topic: &str, count: usize) -> Result<Vec<WebResult>> {
    let query = urlencoding::encode(topic);
    let url = format!("https://html.duckduckgo.com/html/?q={query}");
    let vurl = validate_url(&url, false, false).await?;
    let mut headers = HashMap::new();
    headers.insert("User-Agent".into(), SEARCH_USER_AGENT.into());
    let text = http_get_text(&vurl, Some(&headers), DEFAULT_SEARCH_TIMEOUT).await?;
    parse_duckduckgo_html(&text, count).await
}

async fn web_search(topic: &str, count: usize) -> Result<String> {
    let results = if let Some(results) = ddg_instant_answer(topic, count).await {
        results
    } else {
        web_search_html(topic, count).await?
    };

    if results.is_empty() {
        return Ok(String::new());
    }

    let mut report = String::new();
    for (i, result) in results.iter().enumerate() {
        report.push_str(&format!(
            "{}. {}\n   URL: {}\n   Summary: {}\n\n",
            i + 1,
            result.title,
            result.url,
            result.snippet.replace('\n', " ")
        ));
    }
    Ok(report)
}

async fn parse_duckduckgo_html(html: &str, count: usize) -> Result<Vec<WebResult>> {
    let document = Html::parse_document(html);
    let result_selector = Selector::parse(".result").map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let title_selector = Selector::parse(".result__a").map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let snippet_selector =
        Selector::parse(".result__snippet").map_err(|e| anyhow::anyhow!("{e:?}"))?;

    let mut out = Vec::new();
    for result in document.select(&result_selector).take(count) {
        let mut title = String::new();
        let mut url = String::new();
        if let Some(a) = result.select(&title_selector).next() {
            title = a.text().collect::<Vec<_>>().join(" ").trim().to_string();
            if let Some(href) = a.value().attr("href") {
                url = validated_search_url(href).await.unwrap_or_default();
            }
        }
        let snippet = result
            .select(&snippet_selector)
            .next()
            .map(|a| a.text().collect::<Vec<_>>().join(" ").trim().to_string())
            .unwrap_or_default();
        if !title.is_empty() && !url.is_empty() {
            out.push(WebResult {
                title,
                url,
                snippet,
            });
        }
    }
    Ok(out)
}

fn extract_ddg_url(raw: &str) -> Option<String> {
    let url = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.to_string()
    };
    let parsed = url::Url::parse(&url).ok()?;
    if parsed.host_str() == Some("duckduckgo.com") || parsed.host_str() == Some("r.duckduckgo.com")
    {
        if let Some((_, uddg)) = parsed.query_pairs().find(|(k, _)| k == "uddg") {
            return urlencoding::decode(&uddg)
                .ok()
                .and_then(|s| url::Url::parse(s.as_ref()).ok().map(|_| s.into_owned()));
        }
        return None;
    }
    Some(url)
}

async fn validated_search_url(raw: &str) -> Option<String> {
    let url = extract_ddg_url(raw)?;
    if cfg!(test) {
        let parsed = url::Url::parse(&url).ok()?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return None;
        }
        let host = parsed.host_str()?;
        if host.is_empty() || host == "localhost" {
            return None;
        }
        if host
            .parse::<std::net::IpAddr>()
            .ok()
            .is_some_and(is_non_public_ip)
        {
            return None;
        }
        return Some(url);
    }
    match tokio::time::timeout(URL_VALIDATE_TIMEOUT, validate_url(&url, false, false)).await {
        Ok(Ok(_)) => Some(url),
        _ => None,
    }
}

async fn exec_prompt(model: &str, prompt: &str, yolo: bool, run_id: &str) -> Result<String> {
    let prompt_file = crate::write_prompt_temp(prompt).await?;
    let _prompt_guard = crate::PromptFileGuard(prompt_file.clone());
    let exe = std::env::current_exe()?;
    let mut cmd = tokio::process::Command::new(exe);
    // Limit the patch-generation agent to read-only tools so it cannot modify the repo
    // or run arbitrary commands while still being able to inspect files and references.
    cmd.arg("exec")
        .arg("--model")
        .arg(model)
        .arg("--tools")
        .arg(PATCH_TOOL_POLICY)
        .arg("--prompt-file")
        .arg(&prompt_file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.env("OMGB_RESEARCH_RUN_ID", run_id);
    if yolo {
        cmd.arg("--yolo");
    }
    let (mut child, group) = crate::spawn_with_process_group(cmd)?;
    let mut stdout = child.stdout.take().context("stdout not piped")?;
    let mut stderr = child.stderr.take().context("stderr not piped")?;
    let mut out_capture = crate::BoundedCapture::new(MAX_PROMPT_OUTPUT_BYTES as usize);
    let mut err_capture = crate::BoundedCapture::new(MAX_PROMPT_OUTPUT_BYTES as usize);

    let out_handle = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stdout, &mut out_capture).await;
        out_capture.into_string()
    });
    let err_handle = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stderr, &mut err_capture).await;
        err_capture.into_string()
    });

    let status = match tokio::time::timeout(PATCH_PROMPT_TIMEOUT, child.wait()).await {
        Ok(s) => s?,
        Err(_) => {
            crate::kill_child_and_reap(&mut child, group.as_ref()).await;
            out_handle.abort();
            err_handle.abort();
            bail!(
                "patch generation timed out after {}s",
                PATCH_PROMPT_TIMEOUT.as_secs()
            );
        }
    };
    crate::kill_process_group(group.as_ref());

    let out = out_handle.await.unwrap_or_default();
    let err = err_handle.await.unwrap_or_default();
    if !status.success() {
        bail!("failed to generate patch: {err}");
    }
    Ok(out)
}

pub async fn run_research(
    topic: &str,
    count: usize,
    model: Option<String>,
    yolo: bool,
    output: Option<PathBuf>,
) -> Result<()> {
    if model.is_some() && !yolo {
        bail!("--yolo is required to generate a patch with --model");
    }
    let run_id = uuid::Uuid::new_v4().to_string();
    let research = research_with_provenance(topic, count).await?;
    let report = research.report;
    if report.len() > MAX_RESEARCH_REPORT_BYTES {
        bail!(
            "research report exceeds the {} byte limit",
            MAX_RESEARCH_REPORT_BYTES
        );
    }
    let dir = crate::providers::omg_dir()?.join("research");
    let report_path = match output {
        Some(p) => sanitize_output_path(&dir, &p)?,
        None => dir.join(format!("{}-{run_id}.md", safe_filename(topic))),
    };
    prepare_output_path(&dir, &report_path)?;
    crate::providers::write_file_atomic(&report_path, &report, true)?;
    let report_sha256 = sha256(report.as_bytes());
    let mut artifacts = vec![artifact_manifest(
        &dir,
        &report_path,
        "research_report",
        &report_sha256,
    )?];
    println!("wrote research report to {}", report_path.display());

    let mut patch_generation = None;
    if let Some(model) = model {
        let prompt = format!(
            "Given the following research report, propose a concise patch or implementation plan. Output only the patch content.\n\n{report}"
        );
        let provider_execution_fingerprint =
            crate::providers::provider_execution_fingerprint(&model)?;
        let prompt_sha256 = sha256(prompt.as_bytes());
        let tool_policy_sha256 = sha256(PATCH_TOOL_POLICY.as_bytes());
        match exec_prompt(&model, &prompt, yolo, &run_id).await {
            Ok(patch) => {
                let patch_path = report_path.with_extension("patch");
                prepare_output_path(&dir, &patch_path)?;
                crate::providers::write_file_atomic(&patch_path, &patch, true)?;
                artifacts.push(artifact_manifest(
                    &dir,
                    &patch_path,
                    "model_patch_proposal",
                    &sha256(patch.as_bytes()),
                )?);
                patch_generation = Some(PatchGenerationManifest {
                    model,
                    provider_execution_fingerprint,
                    prompt_sha256,
                    tool_policy_sha256,
                    yolo,
                    outcome: "succeeded",
                    error_sha256: None,
                });
                println!("wrote patch to {}", patch_path.display());
            }
            Err(e) => {
                patch_generation = Some(PatchGenerationManifest {
                    model,
                    provider_execution_fingerprint,
                    prompt_sha256,
                    tool_policy_sha256,
                    yolo,
                    outcome: "failed",
                    error_sha256: Some(sha256(e.to_string().as_bytes())),
                });
                eprintln!("warning: could not generate patch: {e}");
            }
        }
    }

    let manifest = ResearchRunManifest {
        schema_version: RESEARCH_MANIFEST_VERSION,
        run_id,
        created_at: chrono::Utc::now().to_rfc3339(),
        topic_sha256: sha256(topic.as_bytes()),
        topic_bytes: topic.len(),
        requested_count: count,
        effective_count: count.min(MAX_RESULTS),
        sources: research.sources,
        repository: repository_provenance(),
        budget: ResearchBudgetManifest {
            max_results: MAX_RESULTS,
            search_timeout_seconds: DEFAULT_SEARCH_TIMEOUT.as_secs(),
            url_validation_timeout_seconds: URL_VALIDATE_TIMEOUT.as_secs(),
            report_bytes_limit: MAX_RESEARCH_REPORT_BYTES,
            patch_timeout_seconds: PATCH_PROMPT_TIMEOUT.as_secs(),
            patch_output_bytes_limit: MAX_PROMPT_OUTPUT_BYTES,
        },
        artifacts,
        patch_generation,
    };
    let (manifest_path, manifest_sha256) = write_run_manifest(&dir, &manifest)?;
    println!(
        "wrote verified research manifest to {} (sha256 {})",
        manifest_path.display(),
        manifest_sha256
    );
    Ok(())
}

fn parse_atom(text: &str) -> Result<Vec<ArxivEntry>> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);

    let mut entries = Vec::new();
    let mut current: Option<ArxivEntry> = None;
    let mut current_tag = String::new();
    let mut buf = Vec::new();

    loop {
        let event = reader.read_event_into(&mut buf)?;
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if name == "entry" {
                    current = Some(ArxivEntry {
                        title: String::new(),
                        summary: String::new(),
                        id: String::new(),
                        pdf: String::new(),
                        authors: Vec::new(),
                    });
                } else if current.is_some()
                    && name == "link"
                    && let (Some(title), Some(href)) =
                        (attr_value(&e, "title"), attr_value(&e, "href"))
                    && title == "pdf"
                    && let Some(entry) = current.as_mut()
                {
                    entry.pdf = href;
                }
                current_tag = name;
            }
            Event::Text(e) => {
                if let Some(entry) = current.as_mut() {
                    let text = e.decode()?.into_owned();
                    match current_tag.as_str() {
                        "title" => entry.title.push_str(&text),
                        "summary" => entry.summary.push_str(&text),
                        "id" => entry.id.push_str(&text),
                        "name" => entry.authors.push(text),
                        _ => {}
                    }
                }
            }
            Event::CData(e) => {
                if let Some(entry) = current.as_mut() {
                    let text = e.decode()?.into_owned();
                    match current_tag.as_str() {
                        "title" => entry.title.push_str(&text),
                        "summary" => entry.summary.push_str(&text),
                        "id" => entry.id.push_str(&text),
                        "name" => entry.authors.push(text),
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if name == "entry"
                    && let Some(entry) = current.take()
                {
                    entries.push(entry);
                }
                current_tag.clear();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(entries)
}

fn attr_value(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    let attr = e
        .attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name.as_bytes())?;
    Some(String::from_utf8_lossy(attr.value.as_ref()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest(run_id: String, artifact: ResearchArtifactManifest) -> ResearchRunManifest {
        ResearchRunManifest {
            schema_version: RESEARCH_MANIFEST_VERSION,
            run_id,
            created_at: "2026-08-13T00:00:00Z".into(),
            topic_sha256: sha256(b"private research topic"),
            topic_bytes: 22,
            requested_count: 5,
            effective_count: 5,
            sources: vec![
                ResearchSourceManifest {
                    name: "arxiv_atom_v1",
                    outcome: "succeeded",
                    error_sha256: None,
                },
                ResearchSourceManifest {
                    name: "duckduckgo_instant_or_html_v1",
                    outcome: "succeeded",
                    error_sha256: None,
                },
            ],
            repository: ResearchRepositoryProvenance {
                head_commit: Some("a".repeat(40)),
                tracked_dirty: Some(false),
            },
            budget: ResearchBudgetManifest {
                max_results: MAX_RESULTS,
                search_timeout_seconds: DEFAULT_SEARCH_TIMEOUT.as_secs(),
                url_validation_timeout_seconds: URL_VALIDATE_TIMEOUT.as_secs(),
                report_bytes_limit: MAX_RESEARCH_REPORT_BYTES,
                patch_timeout_seconds: PATCH_PROMPT_TIMEOUT.as_secs(),
                patch_output_bytes_limit: MAX_PROMPT_OUTPUT_BYTES,
            },
            artifacts: vec![artifact],
            patch_generation: None,
        }
    }

    #[test]
    fn test_parse_atom() {
        let xml = r#"<feed>
            <entry>
                <title>Test Paper</title>
                <summary>A test summary.</summary>
                <id>http://arxiv.org/abs/1234.5678</id>
                <link href="http://arxiv.org/pdf/1234.5678.pdf" title="pdf" />
                <author><name>A. Tester</name></author>
            </entry>
        </feed>"#;
        let entries = parse_atom(xml).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Test Paper");
        assert_eq!(entries[0].authors, vec!["A. Tester"]);
        assert!(entries[0].pdf.contains("pdf"));
    }

    #[test]
    fn test_parse_atom_with_cdata() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom">
            <entry>
                <title><![CDATA[CDATA Paper]]></title>
                <summary>A test summary.</summary>
                <id>http://arxiv.org/abs/5678.1234</id>
                <link href="http://arxiv.org/pdf/5678.1234.pdf" title="pdf" />
                <author><name>B. CData</name></author>
            </entry>
        </feed>"#;
        let entries = parse_atom(xml).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "CDATA Paper");
        assert_eq!(entries[0].authors, vec!["B. CData"]);
        assert!(entries[0].pdf.contains("5678.1234"));
    }

    #[test]
    fn test_safe_filename_sanitizes_path_chars() {
        assert_eq!(safe_filename("AI/ML: a study"), "AI_ML_a_study");
        assert_eq!(safe_filename("../../etc/passwd"), "etc_passwd");
        assert_eq!(safe_filename("---."), "report");
    }

    #[test]
    fn test_sanitize_output_path_blocks_traversal() {
        let dir = std::path::Path::new("/home/user/.omgb/research");
        assert!(sanitize_output_path(dir, std::path::Path::new("report.md")).is_ok());
        assert!(sanitize_output_path(dir, std::path::Path::new("../passwd")).is_err());
        assert!(sanitize_output_path(dir, std::path::Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn prepare_output_path_rejects_non_file_targets() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("research");
        let target = root.join("report.md");
        std::fs::create_dir_all(&target).unwrap();
        assert!(prepare_output_path(&root, &target).is_err());
    }

    #[test]
    fn artifact_manifest_verifies_content_and_relative_path() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("report.md");
        std::fs::write(&path, "verified report").unwrap();
        let artifact = artifact_manifest(
            temp.path(),
            &path,
            "research_report",
            &sha256(b"verified report"),
        )
        .unwrap();
        assert_eq!(artifact.relative_path, "report.md");
        assert_eq!(artifact.bytes, 15);
        assert!(
            artifact_manifest(temp.path(), &path, "research_report", &sha256(b"tampered")).is_err()
        );
    }

    #[test]
    fn run_manifest_is_unique_verified_and_does_not_persist_raw_topic() {
        let temp = tempfile::tempdir().unwrap();
        let report = temp.path().join("report.md");
        std::fs::write(&report, "report").unwrap();
        let artifact =
            artifact_manifest(temp.path(), &report, "research_report", &sha256(b"report")).unwrap();
        let manifest = test_manifest(uuid::Uuid::new_v4().to_string(), artifact);
        let (path, expected) = write_run_manifest(temp.path(), &manifest).unwrap();
        let (actual, _) = hash_file(&path).unwrap();
        assert_eq!(actual, expected);
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(!raw.contains("private research topic"));
        assert!(raw.contains("topic_sha256"));
        assert!(write_run_manifest(temp.path(), &manifest).is_err());
    }

    #[test]
    fn test_extract_ddg_url() {
        assert_eq!(
            extract_ddg_url("https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com"),
            Some("https://example.com".into())
        );
        assert_eq!(
            extract_ddg_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com"),
            Some("https://example.com".into())
        );
        assert_eq!(
            extract_ddg_url("https://example.com"),
            Some("https://example.com".into())
        );
    }

    #[tokio::test]
    async fn test_parse_duckduckgo_html_extracts_results() {
        let html = r#"<!DOCTYPE html>
<html><body>
<div class="result">
    <a class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com">Example</a>
    <a class="result__snippet">This is an example page.</a>
</div>
</body></html>"#;
        let results = parse_duckduckgo_html(html, 5).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example");
        assert_eq!(results[0].url, "https://example.com");
        assert_eq!(results[0].snippet, "This is an example page.");
    }
}
