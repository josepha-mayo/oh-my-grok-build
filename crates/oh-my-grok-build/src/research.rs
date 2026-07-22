//! Deep arXiv/web research for `omgb`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, bail};
use quick_xml::Reader;
use quick_xml::events::Event;
use scraper::{Html, Selector};

use crate::net::{http_get_text, validate_url};

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
const MAX_RESULTS: usize = 100;

pub async fn research(topic: &str, count: usize) -> Result<String> {
    let count = count.min(MAX_RESULTS);
    let mut report = format!("Research: {}\n\n", topic);
    let mut found = false;

    match arxiv_research(topic, count).await {
        Ok(text) => {
            report.push_str(&text);
            found = true;
        }
        Err(e) => report.push_str(&format!("arXiv search unavailable: {e}\n\n")),
    }

    match web_search(topic, count).await {
        Ok(text) => {
            if !text.is_empty() {
                report.push_str(&format!("\nWeb results:\n\n{text}"));
                found = true;
            }
        }
        Err(e) => report.push_str(&format!("\nWeb search unavailable: {e}\n")),
    }

    if !found {
        bail!("no research results for '{topic}'");
    }
    Ok(report)
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
    validate_url(&url, false, false).await.ok().map(|_| url)
}

async fn exec_prompt(model: &str, prompt: &str, yolo: bool) -> Result<String> {
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
        .arg("read_file,grep,list_dir,web_search,web_fetch")
        .arg("--prompt-file")
        .arg(&prompt_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if yolo {
        cmd.arg("--yolo");
    }
    let out = cmd.output().await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("failed to generate patch: {stderr}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub async fn run_research(
    topic: &str,
    count: usize,
    model: Option<String>,
    yolo: bool,
    output: Option<PathBuf>,
) -> Result<()> {
    let report = research(topic, count).await?;
    let dir = crate::providers::omg_dir()?.join("research");
    let report_path = match output {
        Some(p) => sanitize_output_path(&dir, &p)?,
        None => dir.join(format!("{}.md", safe_filename(topic))),
    };
    if let Some(parent) = report_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::providers::write_file_atomic(&report_path, &report, true)?;
    println!("wrote research report to {}", report_path.display());

    if let Some(model) = model {
        if !yolo {
            bail!("--yolo is required to generate a patch with --model");
        }
        let prompt = format!(
            "Given the following research report, propose a concise patch or implementation plan. Output only the patch content.\n\n{report}"
        );
        match exec_prompt(&model, &prompt, yolo).await {
            Ok(patch) => {
                let patch_path = report_path.with_extension("patch");
                crate::providers::write_file_atomic(&patch_path, &patch, true)?;
                println!("wrote patch to {}", patch_path.display());
            }
            Err(e) => {
                eprintln!("warning: could not generate patch: {e}");
            }
        }
    }
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
