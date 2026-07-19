//! Deep arXiv/web research for `omgb`.

use std::path::PathBuf;
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
    let url = validate_url(&url, false).await?;
    let text = http_get_text(&url, Duration::from_secs(30)).await?;
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

pub async fn run_research(topic: &str, count: usize, output: Option<PathBuf>) -> Result<()> {
    let report = research(topic, count).await?;
    if let Some(path) = output {
        std::fs::write(&path, &report)?;
        println!("wrote research report to {}", path.display());
    } else {
        println!("{report}");
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
