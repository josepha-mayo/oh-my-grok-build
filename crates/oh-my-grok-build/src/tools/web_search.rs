use anyhow::Result;
use xai_grok_tools::types::output::WebSearchOutput;
use xai_grok_tools::types::requirements::{Expr, ToolRequirement};
use xai_grok_tools::types::tool::{ToolKind, ToolNamespace};
use xai_grok_tools::types::tool_metadata::ToolMetadata;
use xai_tool_protocol::{ToolCapabilities, ToolId};
use xai_tool_runtime::Tool;
use xai_tool_runtime::context::{ListToolsContext, ToolCallContext};
use xai_tool_runtime::error::ToolError;
use xai_tool_types::ToolDescription;

pub use xai_grok_tools::implementations::grok_build::web_search::WebSearchInput;

#[derive(Debug, Default)]
pub struct OmgbWebSearchTool;

impl ToolMetadata for OmgbWebSearchTool {
    fn kind(&self) -> ToolKind {
        ToolKind::WebSearch
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Search the web for up-to-date information using one of several configured providers (Tavily, Brave, Serper, Google, Bing, SearXNG, DuckDuckGo)."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl Tool for OmgbWebSearchTool {
    type Args = WebSearchInput;
    type Output = WebSearchOutput;

    fn id(&self) -> ToolId {
        ToolId::new("web_search").expect("valid tool id")
    }

    fn description(&self, _ctx: &ListToolsContext) -> ToolDescription {
        ToolDescription::new("web_search", ToolMetadata::description_template(self))
    }

    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(xai_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    async fn run(
        &self,
        _ctx: ToolCallContext,
        input: WebSearchInput,
    ) -> Result<WebSearchOutput, ToolError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ToolError::execution(self.id(), e.to_string()))?;

        let query = input.query.trim();
        if query.is_empty() {
            return Err(ToolError::execution(
                self.id(),
                "empty search query".to_string(),
            ));
        }

        let allowed = input.allowed_domains.as_deref();
        let mut errors: Vec<String> = Vec::new();

        if let Some(key) = std::env::var("TAVILY_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
        {
            match tavily(&client, &key, query, allowed).await {
                Ok(out) => return Ok(out),
                Err(e) => errors.push(e.to_string()),
            }
        }
        if let Some(key) = std::env::var("BRAVE_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
        {
            match brave(&client, &key, query, allowed).await {
                Ok(out) => return Ok(out),
                Err(e) => errors.push(e.to_string()),
            }
        }
        if let Some(key) = std::env::var("SERPER_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
        {
            match serper(&client, &key, query, allowed).await {
                Ok(out) => return Ok(out),
                Err(e) => errors.push(e.to_string()),
            }
        }
        if let (Some(key), Some(cx)) = (
            std::env::var("GOOGLE_API_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            std::env::var("GOOGLE_CX").ok().filter(|s| !s.is_empty()),
        ) {
            match google(&client, &key, &cx, query, allowed).await {
                Ok(out) => return Ok(out),
                Err(e) => errors.push(e.to_string()),
            }
        }
        if let Some(key) = std::env::var("BING_API_KEY").ok().filter(|s| !s.is_empty()) {
            match bing(&client, &key, query, allowed).await {
                Ok(out) => return Ok(out),
                Err(e) => errors.push(e.to_string()),
            }
        }
        if let Some(base) = std::env::var("SEARXNG_URL").ok().filter(|s| !s.is_empty()) {
            match searxng(&client, &base, query, allowed).await {
                Ok(out) => return Ok(out),
                Err(e) => errors.push(e.to_string()),
            }
        }
        match duckduckgo(&client, query, allowed).await {
            Ok(out) => return Ok(out),
            Err(e) => errors.push(e.to_string()),
        }

        Err(ToolError::execution(
            self.id(),
            errors
                .last()
                .cloned()
                .unwrap_or_else(|| "no web search provider configured".to_string()),
        ))
    }
}

fn apply_allowed(out: WebSearchOutput, allowed: Option<&[String]>) -> WebSearchOutput {
    if let Some(domains) = allowed {
        let filtered: Vec<String> = out
            .citations
            .iter()
            .filter(|url| {
                url::Url::parse(url)
                    .ok()
                    .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
                    .is_some_and(|h| {
                        domains
                            .iter()
                            .any(|d| h == *d || h.ends_with(&format!(".{d}")))
                    })
            })
            .cloned()
            .collect();
        WebSearchOutput {
            citations: filtered,
            ..out
        }
    } else {
        out
    }
}

fn build_output(query: &str, items: &[(String, String, String)]) -> WebSearchOutput {
    let mut content = String::new();
    let mut citations = Vec::new();
    for (title, url, snippet) in items {
        content.push_str(&format!("## {title}\n{snippet}\n<{url}>\n\n"));
        citations.push(url.clone());
    }
    WebSearchOutput {
        query: query.to_string(),
        content: content.trim().to_string(),
        citations,
        allowed_domains: None,
        pre_formatted: None,
    }
}

async fn tavily(
    client: &reqwest::Client,
    key: &str,
    query: &str,
    allowed: Option<&[String]>,
) -> Result<WebSearchOutput> {
    let resp: serde_json::Value = client
        .post("https://api.tavily.com/search")
        .json(&serde_json::json!({
            "query": query,
            "api_key": key,
            "search_depth": "basic",
            "max_results": 10,
        }))
        .send()
        .await?
        .json()
        .await?;
    let mut items = Vec::new();
    if let Some(results) = resp.get("results").and_then(|v| v.as_array()) {
        for r in results {
            let title = r
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = r
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let text = r
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !url.is_empty() {
                items.push((title, url, text));
            }
        }
    }
    Ok(apply_allowed(build_output(query, &items), allowed))
}

async fn brave(
    client: &reqwest::Client,
    key: &str,
    query: &str,
    allowed: Option<&[String]>,
) -> Result<WebSearchOutput> {
    let resp: serde_json::Value = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .query(&[("q", query), ("count", "10")])
        .header("X-Subscription-Token", key)
        .send()
        .await?
        .json()
        .await?;
    let mut items = Vec::new();
    if let Some(results) = resp
        .get("web")
        .and_then(|v| v.get("results"))
        .and_then(|v| v.as_array())
    {
        for r in results {
            let title = r
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = r
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let desc = r
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !url.is_empty() {
                items.push((title, url, desc));
            }
        }
    }
    Ok(apply_allowed(build_output(query, &items), allowed))
}

async fn serper(
    client: &reqwest::Client,
    key: &str,
    query: &str,
    allowed: Option<&[String]>,
) -> Result<WebSearchOutput> {
    let resp: serde_json::Value = client
        .post("https://google.serper.dev/search")
        .header("X-API-KEY", key)
        .json(&serde_json::json!({ "q": query }))
        .send()
        .await?
        .json()
        .await?;
    let mut items = Vec::new();
    if let Some(results) = resp.get("organic").and_then(|v| v.as_array()) {
        for r in results {
            let title = r
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let link = r
                .get("link")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = r
                .get("snippet")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !link.is_empty() {
                items.push((title, link, snippet));
            }
        }
    }
    Ok(apply_allowed(build_output(query, &items), allowed))
}

async fn google(
    client: &reqwest::Client,
    key: &str,
    cx: &str,
    query: &str,
    allowed: Option<&[String]>,
) -> Result<WebSearchOutput> {
    let resp: serde_json::Value = client
        .get("https://www.googleapis.com/customsearch/v1")
        .query(&[("key", key), ("cx", cx), ("q", query)])
        .send()
        .await?
        .json()
        .await?;
    let mut items = Vec::new();
    if let Some(arr) = resp.get("items").and_then(|v| v.as_array()) {
        for r in arr {
            let title = r
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let link = r
                .get("link")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = r
                .get("snippet")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !link.is_empty() {
                items.push((title, link, snippet));
            }
        }
    }
    Ok(apply_allowed(build_output(query, &items), allowed))
}

async fn bing(
    client: &reqwest::Client,
    key: &str,
    query: &str,
    allowed: Option<&[String]>,
) -> Result<WebSearchOutput> {
    let resp: serde_json::Value = client
        .get("https://api.bing.microsoft.com/v7.0/search")
        .query(&[("q", query), ("count", "10")])
        .header("Ocp-Apim-Subscription-Key", key)
        .send()
        .await?
        .json()
        .await?;
    let mut items = Vec::new();
    if let Some(pages) = resp
        .get("webPages")
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_array())
    {
        for r in pages {
            let name = r
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = r
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = r
                .get("snippet")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !url.is_empty() {
                items.push((name, url, snippet));
            }
        }
    }
    Ok(apply_allowed(build_output(query, &items), allowed))
}

async fn searxng(
    client: &reqwest::Client,
    base: &str,
    query: &str,
    allowed: Option<&[String]>,
) -> Result<WebSearchOutput> {
    let url = format!("{base}/search");
    let resp: serde_json::Value = client
        .get(&url)
        .query(&[("q", query), ("format", "json")])
        .send()
        .await?
        .json()
        .await?;
    let mut items = Vec::new();
    if let Some(results) = resp.get("results").and_then(|v| v.as_array()) {
        for r in results {
            let title = r
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = r
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = r
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !url.is_empty() {
                items.push((title, url, content));
            }
        }
    }
    Ok(apply_allowed(build_output(query, &items), allowed))
}

async fn duckduckgo(
    client: &reqwest::Client,
    query: &str,
    allowed: Option<&[String]>,
) -> Result<WebSearchOutput> {
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );
    let text = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (compatible; grok-agent/1.0)")
        .send()
        .await?
        .text()
        .await?;
    let doc = scraper::Html::parse_document(&text);
    let sel = scraper::Selector::parse(".result")
        .map_err(|e| anyhow::anyhow!("invalid result selector: {e}"))?;
    let a_sel = scraper::Selector::parse(".result__a")
        .map_err(|e| anyhow::anyhow!("invalid link selector: {e}"))?;
    let snip_sel = scraper::Selector::parse(".result__snippet")
        .map_err(|e| anyhow::anyhow!("invalid snippet selector: {e}"))?;
    let mut items = Vec::new();
    for el in doc.select(&sel) {
        let title = el
            .select(&a_sel)
            .next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();
        let url = el
            .select(&a_sel)
            .next()
            .and_then(|e| e.value().attr("href"))
            .unwrap_or("")
            .to_string();
        let snippet = el
            .select(&snip_sel)
            .next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();
        let real_url = if let Some(rest) = url.strip_prefix("/uddg=") {
            urlencoding::decode(rest)
                .map_err(|e| anyhow::anyhow!("decode ddg url: {e}"))?
                .to_string()
        } else {
            url
        };
        if !real_url.is_empty() {
            items.push((title, real_url, snippet));
        }
    }
    Ok(apply_allowed(build_output(query, &items), allowed))
}
