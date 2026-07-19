//! Deep arXiv/web research for `omgb`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::net::{http_get_text, validate_url};

#[derive(Debug)]
struct ArxivEntry {
    title: String,
    summary: String,
    id: String,
    pdf: String,
    authors: Vec<String>,
}

pub async fn research(topic: &str, count: usize) -> Result<String> {
    let query = urlencoding::encode(topic);
    let url = format!(
        "http://export.arxiv.org/api/query?search_query=all:{query}&start=0&max_results={count}&sortBy=relevance&sortOrder=descending"
    );
    let vurl = validate_url(&url, false).await?;
    let text = http_get_text(&vurl, Duration::from_secs(30)).await?;
    let entries = parse_atom(&text).unwrap_or_default();

    if entries.is_empty() {
        bail!("no arXiv results for '{topic}'");
    }

    let mut report = format!("Research: {}\n\n", topic);
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

async fn exec_prompt(model: &str, prompt: &str) -> Result<String> {
    let exe = std::env::current_exe()?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("exec")
        .arg(prompt)
        .arg("--model")
        .arg(model)
        .arg("--yolo")
        .env("OMGB_EXEC_CAPTURE", "1")
        .stdout(Stdio::piped());
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
    output: Option<PathBuf>,
) -> Result<()> {
    let report = research(topic, count).await?;
    let dir = crate::providers::omg_dir().join("research");
    std::fs::create_dir_all(&dir)?;
    let report_path = output.unwrap_or_else(|| dir.join(format!("{topic}.md")));
    std::fs::write(&report_path, &report)?;
    println!("wrote research report to {}", report_path.display());

    if let Some(model) = model {
        let prompt = format!(
            "Given the following research report, propose a concise patch or implementation plan. Output only the patch content.\n\n{report}"
        );
        match exec_prompt(&model, &prompt).await {
            Ok(patch) => {
                let patch_path = report_path.with_extension("patch");
                std::fs::write(&patch_path, &patch)?;
                println!("wrote patch to {}", patch_path.display());
            }
            Err(e) => {
                eprintln!("warning: could not generate patch: {e}");
            }
        }
    }
    Ok(())
}

fn parse_atom(text: &str) -> Option<Vec<ArxivEntry>> {
    let mut entries = Vec::new();
    for chunk in text.split("<entry>") {
        let Some(end) = chunk.find("</entry>") else {
            continue;
        };
        let entry_xml = &chunk[..end];
        let title = first_tag(entry_xml, "title").unwrap_or_default();
        let summary = first_tag(entry_xml, "summary").unwrap_or_default();
        let id = first_tag(entry_xml, "id").unwrap_or_default();
        let pdf = entry_xml
            .lines()
            .filter(|l| l.contains("<link") && l.contains("title=\"pdf\""))
            .filter_map(|l| extract_attr(l, "href"))
            .next()
            .unwrap_or_else(|| id.clone());
        let authors: Vec<_> = entry_xml
            .lines()
            .filter(|l| l.contains("<name>"))
            .filter_map(|l| first_tag(l, "name"))
            .collect();

        entries.push(ArxivEntry {
            title,
            summary,
            id,
            pdf,
            authors,
        });
    }
    Some(entries)
}

fn first_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)?;
    Some(strip_cdata(&text[start..start + end]).trim().to_string())
}

fn extract_attr(text: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = text.find(&needle)? + needle.len();
    let end = text[start..].find('"')?;
    Some(text[start..start + end].to_string())
}

fn strip_cdata(s: &str) -> String {
    s.replace("<![CDATA[", "").replace("]]>", "")
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
    fn test_first_tag_cdata() {
        let text = "<title><![CDATA[Hello]]></title>";
        assert_eq!(first_tag(text, "title").unwrap(), "Hello");
    }
}
