//! BYOK and local-model provider management for `omgb`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::args::{AddProviderArgs, ApiBackend, DiscoverArgs};
use crate::net::{http_get_text, http_post_json, validate_url};

const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434/v1";
const DEFAULT_LMSTUDIO_URL: &str = "http://localhost:1234/v1";

pub fn omg_dir() -> PathBuf {
    std::env::var("OMGB_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".omgb"))
}

fn omg_config_path() -> PathBuf {
    omg_dir().join("config.json")
}

fn omg_env_path() -> PathBuf {
    omg_dir().join(".env")
}

fn grok_home() -> PathBuf {
    xai_grok_shell::util::grok_home::grok_home()
}

fn grok_config_path() -> PathBuf {
    grok_home().join("config.toml")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OmgConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub model: String,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_key: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u64>,
}

pub fn load_omg_config() -> Result<OmgConfig> {
    let path = omg_config_path();
    if !path.exists() {
        return Ok(OmgConfig::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

pub fn save_omg_config(config: &OmgConfig) -> Result<()> {
    let dir = omg_dir();
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("config.json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(config)?)?;
    std::fs::rename(&tmp, omg_config_path())?;
    Ok(())
}

fn load_env_file() -> HashMap<String, String> {
    let path = omg_env_path();
    let mut map = HashMap::new();
    if let Ok(raw) = std::fs::read_to_string(&path) {
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    map
}

fn env_var_name(provider_id: &str) -> String {
    format!(
        "OMGB_{}_API_KEY",
        provider_id.replace('-', "_").to_uppercase()
    )
}

fn api_key_target_var(provider_id: &str, env_keys: Option<&[String]>) -> String {
    env_keys
        .and_then(|keys| keys.iter().find(|k| k.ends_with("_API_KEY")).cloned())
        .unwrap_or_else(|| env_var_name(provider_id))
}

pub fn write_api_key(provider_id: &str, env_keys: Option<&[String]>, key: &str) -> Result<String> {
    let target = api_key_target_var(provider_id, env_keys);
    let legacy = env_var_name(provider_id);
    let dir = omg_dir();
    std::fs::create_dir_all(&dir)?;
    let path = omg_env_path();
    let prefixes: Vec<String> = if target == legacy {
        vec![format!("{target}=")]
    } else {
        vec![format!("{target}="), format!("{legacy}=")]
    };
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !prefixes.iter().any(|p| l.trim().starts_with(p)))
        .map(|l| l.to_string())
        .collect();
    lines.push(format!("{target}={key}"));
    std::fs::write(&path, lines.join("\n") + "\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(target)
}

pub fn remove_api_key(provider_id: &str, env_keys: Option<&[String]>) -> Result<()> {
    let path = omg_env_path();
    if !path.exists() {
        return Ok(());
    }
    let target = api_key_target_var(provider_id, env_keys);
    let legacy = env_var_name(provider_id);
    let prefixes: Vec<String> = if target == legacy {
        vec![format!("{target}=")]
    } else {
        vec![format!("{target}="), format!("{legacy}=")]
    };
    let raw = std::fs::read_to_string(&path)?;
    let lines: Vec<String> = raw
        .lines()
        .filter(|l| !prefixes.iter().any(|p| l.trim().starts_with(p)))
        .map(|l| l.to_string())
        .collect();
    let content = lines.join("\n").trim_end().to_string();
    std::fs::write(
        &path,
        if content.is_empty() {
            content
        } else {
            content + "\n"
        },
    )?;
    Ok(())
}

pub fn resolve_api_key(provider: &ProviderConfig) -> Option<String> {
    let keys: Vec<String> = provider.env_key.clone().unwrap_or_else(|| {
        if provider.id.starts_with("ollama")
            || provider.id.starts_with("lmstudio")
            || provider.id.starts_with("local-")
        {
            vec!["OPENAI_API_KEY".to_string()]
        } else {
            vec![env_var_name(&provider.id)]
        }
    });
    for k in &keys {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    let dotenv = load_env_file();
    for k in &keys {
        if let Some(v) = dotenv.get(k) {
            if !v.is_empty() {
                return Some(v.clone());
            }
        }
    }
    None
}

pub fn resolve_env_key(key: &str) -> Option<String> {
    if let Ok(v) = std::env::var(key) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    let dotenv = load_env_file();
    dotenv.get(key).filter(|v| !v.is_empty()).cloned()
}

pub fn list_providers() -> Result<Vec<ProviderConfig>> {
    let cfg = load_omg_config()?;
    Ok(cfg.providers.values().cloned().collect())
}

pub fn get_provider(id: &str) -> Result<Option<ProviderConfig>> {
    let cfg = load_omg_config()?;
    Ok(cfg.providers.get(id).cloned())
}

pub fn remove_provider(id: &str) -> Result<()> {
    let mut cfg = load_omg_config()?;
    let provider = cfg.providers.remove(id);
    if cfg.default_model.as_deref() == Some(&format!("omgb-{id}")) {
        cfg.default_model = None;
    }
    save_omg_config(&cfg)?;
    remove_api_key(id, provider.as_ref().and_then(|p| p.env_key.as_deref()))?;
    remove_provider_from_grok_config(id)?;
    Ok(())
}

fn provider_template(id: &str) -> Option<ProviderConfig> {
    match id {
        "openai" => Some(ProviderConfig {
            id: "openai".into(),
            name: "OpenAI".into(),
            model: "gpt-4o".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_backend: Some("chat_completions".into()),
            env_key: Some(vec!["OPENAI_API_KEY".into()]),
            extra_headers: None,
            context_window: Some(200_000),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }),
        "anthropic" => Some(ProviderConfig {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            model: "claude-3-5-sonnet-20241022".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            api_backend: Some("messages".into()),
            env_key: Some(vec!["ANTHROPIC_API_KEY".into()]),
            extra_headers: Some(HashMap::from([(
                "anthropic-version".into(),
                "2023-06-01".into(),
            )])),
            context_window: Some(200_000),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }),
        "xai" => Some(ProviderConfig {
            id: "xai".into(),
            name: "xAI".into(),
            model: "grok-4.5".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_backend: Some("chat_completions".into()),
            env_key: Some(vec!["XAI_API_KEY".into()]),
            extra_headers: None,
            context_window: Some(500_000),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }),
        "openrouter" => Some(ProviderConfig {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            model: "anthropic/claude-3.5-sonnet".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_backend: Some("chat_completions".into()),
            env_key: Some(vec!["OPENROUTER_API_KEY".into()]),
            extra_headers: Some(HashMap::from([
                ("HTTP-Referer".into(), "https://oh-my-grok.build".into()),
                ("X-Title".into(), "oh-my-grok-build".into()),
            ])),
            context_window: Some(200_000),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }),
        "ollama" => Some(ProviderConfig {
            id: "ollama".into(),
            name: "Ollama (local)".into(),
            model: "codellama".into(),
            base_url: DEFAULT_OLLAMA_URL.into(),
            api_backend: Some("chat_completions".into()),
            env_key: None,
            extra_headers: None,
            context_window: Some(128_000),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }),
        "lmstudio" => Some(ProviderConfig {
            id: "lmstudio".into(),
            name: "LM Studio (local)".into(),
            model: "local-model".into(),
            base_url: DEFAULT_LMSTUDIO_URL.into(),
            api_backend: Some("chat_completions".into()),
            env_key: Some(vec!["LMSTUDIO_API_KEY".into(), "OPENAI_API_KEY".into()]),
            extra_headers: None,
            context_window: Some(128_000),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }),
        "vllm" => Some(ProviderConfig {
            id: "vllm".into(),
            name: "vLLM".into(),
            model: String::new(),
            base_url: "http://localhost:8000/v1".into(),
            api_backend: Some("chat_completions".into()),
            env_key: Some(vec!["OMGB_VLLM_API_KEY".into()]),
            extra_headers: None,
            context_window: Some(128_000),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }),
        "llama-cpp" => Some(ProviderConfig {
            id: "llama-cpp".into(),
            name: "llama.cpp server".into(),
            model: String::new(),
            base_url: "http://localhost:8080/v1".into(),
            api_backend: Some("chat_completions".into()),
            env_key: Some(vec!["OMGB_LLAMA_CPP_API_KEY".into()]),
            extra_headers: None,
            context_window: Some(128_000),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }),
        "tabby" => Some(ProviderConfig {
            id: "tabby".into(),
            name: "TabbyAPI".into(),
            model: String::new(),
            base_url: "http://localhost:5000/v1".into(),
            api_backend: Some("chat_completions".into()),
            env_key: Some(vec!["OMGB_TABBY_API_KEY".into()]),
            extra_headers: None,
            context_window: Some(128_000),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }),
        _ => None,
    }
}

pub fn add_provider(args: &AddProviderArgs) -> Result<ProviderConfig> {
    let id = sanitize_provider_id(&args.id);
    if id.is_empty() {
        bail!("provider id is required");
    }

    let mut cfg = load_omg_config()?;
    let mut provider = if let Some(t) = &args.template {
        provider_template(t).ok_or_else(|| anyhow::anyhow!("unknown template '{t}'"))?
    } else {
        let base_url = args
            .base_url
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--base-url is required for custom providers"))?;
        ProviderConfig {
            id: id.clone(),
            name: args.name.clone().unwrap_or_else(|| id.clone()),
            model: args
                .model
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--model is required"))?,
            base_url,
            api_backend: Some(args.backend.as_str().into()),
            env_key: None,
            extra_headers: None,
            context_window: None,
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }
    };

    if let Some(name) = &args.name {
        provider.name = name.clone();
    }
    if let Some(model) = &args.model {
        provider.model = model.clone();
    }
    if let Some(base_url) = &args.base_url {
        provider.base_url = base_url.clone();
    }
    if let Some(env_key) = &args.env_key {
        provider.env_key = Some(vec![env_key.clone()]);
    }
    provider.id = id.clone();
    provider.api_backend = Some(args.backend.as_str().into());

    if let Some(key) = &args.api_key {
        let target = write_api_key(&id, provider.env_key.as_deref(), key)?;
        if provider.env_key.is_none() {
            provider.env_key = Some(vec![target]);
        }
    }

    if args.default || cfg.default_model.is_none() {
        cfg.default_model = Some(format!("omgb-{id}"));
    }
    cfg.providers.insert(id.clone(), provider.clone());
    save_omg_config(&cfg)?;
    sync_provider_to_grok_config(&provider)?;
    Ok(provider)
}

pub fn set_default_provider(id: &str) -> Result<()> {
    let mut cfg = load_omg_config()?;
    if !cfg.providers.contains_key(id) {
        bail!("provider '{id}' not found");
    }
    cfg.default_model = Some(format!("omgb-{id}"));
    save_omg_config(&cfg)?;

    let mut gcfg = load_grok_config_table()?;
    let models = gcfg
        .entry("models")
        .or_insert(toml::Value::Table(toml::map::Map::new()));
    if let toml::Value::Table(m) = models {
        m.insert(
            "default".to_string(),
            toml::Value::String(format!("omgb-{id}")),
        );
    }
    save_grok_config_table(&gcfg)?;
    Ok(())
}

fn sync_provider_to_grok_config(provider: &ProviderConfig) -> Result<()> {
    let mut gcfg = load_grok_config_table()?;
    let model_key = format!("omgb-{}", provider.id);

    let mut section = toml::map::Map::new();
    section.insert("model".into(), toml::Value::String(provider.model.clone()));
    section.insert(
        "base_url".into(),
        toml::Value::String(provider.base_url.clone()),
    );
    section.insert("name".into(), toml::Value::String(provider.name.clone()));
    if let Some(backend) = &provider.api_backend {
        section.insert("api_backend".into(), toml::Value::String(backend.clone()));
    }
    if let Some(keys) = &provider.env_key {
        if !keys.is_empty() {
            section.insert("env_key".into(), toml::Value::String(keys[0].clone()));
        }
    }
    if let Some(headers) = &provider.extra_headers {
        let mut h = toml::map::Map::new();
        for (k, v) in headers {
            h.insert(k.clone(), toml::Value::String(v.clone()));
        }
        section.insert("extra_headers".into(), toml::Value::Table(h));
    }
    if let Some(cw) = provider.context_window {
        section.insert("context_window".into(), toml::Value::Integer(cw as i64));
    }

    let model = gcfg
        .entry("model")
        .or_insert(toml::Value::Table(toml::map::Map::new()));
    if let toml::Value::Table(m) = model {
        m.insert(model_key.clone(), toml::Value::Table(section));
    }

    let models = gcfg
        .entry("models")
        .or_insert(toml::Value::Table(toml::map::Map::new()));
    if let toml::Value::Table(m) = models {
        if !m.contains_key("default") {
            m.insert("default".into(), toml::Value::String(model_key));
        }
    }

    save_grok_config_table(&gcfg)?;
    Ok(())
}

fn remove_provider_from_grok_config(id: &str) -> Result<()> {
    let mut gcfg = load_grok_config_table()?;
    let model_key = format!("omgb-{id}");
    if let Some(toml::Value::Table(m)) = gcfg.get_mut("model") {
        m.remove(&model_key);
    }
    if let Some(toml::Value::Table(m)) = gcfg.get_mut("models") {
        if m.get("default").and_then(|v| v.as_str()) == Some(&model_key) {
            let remaining: Vec<String> = gcfg
                .get("model")
                .and_then(|v| v.as_table())
                .map(|t| t.keys().cloned().collect())
                .unwrap_or_default();
            if let Some(first) = remaining.first() {
                m.insert("default".into(), toml::Value::String(first.clone()));
            } else {
                m.remove("default");
            }
        }
    }
    save_grok_config_table(&gcfg)?;
    Ok(())
}

fn load_grok_config_table() -> Result<toml::map::Map<String, toml::Value>> {
    let path = grok_config_path();
    if !path.exists() {
        return Ok(toml::map::Map::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    let value: toml::Value = toml::from_str(&raw)?;
    value
        .as_table()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("grok config is not a table"))
}

fn save_grok_config_table(table: &toml::map::Map<String, toml::Value>) -> Result<()> {
    let path = grok_config_path();
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let raw = toml::to_string_pretty(&toml::Value::Table(table.clone()))?;
    std::fs::write(&path, raw)?;
    Ok(())
}

pub async fn discover_local_models(args: &DiscoverArgs) -> Result<Vec<(String, Vec<String>)>> {
    let mut out = Vec::new();
    let ollama = args.ollama_url.as_deref().unwrap_or(DEFAULT_OLLAMA_URL);
    let lmstudio = args.lmstudio_url.as_deref().unwrap_or(DEFAULT_LMSTUDIO_URL);

    if let Some(models) =
        fetch_model_list(ollama, None, "chat_completions", &HashMap::new(), true).await
    {
        if !models.is_empty() {
            out.push(("ollama".into(), models));
        }
    }
    if let Some(models) =
        fetch_model_list(lmstudio, None, "chat_completions", &HashMap::new(), true).await
    {
        if !models.is_empty() {
            out.push(("lmstudio".into(), models));
        }
    }
    Ok(out)
}

pub fn add_discovered_providers(discovered: &[(String, Vec<String>)]) -> Result<()> {
    let mut cfg = load_omg_config()?;
    for (provider, models) in discovered {
        if models.is_empty() {
            continue;
        }
        let base_url = match provider.as_str() {
            "ollama" => DEFAULT_OLLAMA_URL,
            _ => DEFAULT_LMSTUDIO_URL,
        };
        let id = provider.clone();
        let config = ProviderConfig {
            id: id.clone(),
            name: format!("{provider} (local)"),
            model: models[0].clone(),
            base_url: base_url.into(),
            api_backend: Some("chat_completions".into()),
            env_key: None,
            extra_headers: None,
            context_window: Some(128_000),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        };
        cfg.providers.insert(id, config.clone());
        sync_provider_to_grok_config(&config)?;
    }
    save_omg_config(&cfg)?;
    Ok(())
}

async fn fetch_model_list(
    base_url: &str,
    api_key: Option<&str>,
    backend: &str,
    extra_headers: &HashMap<String, String>,
    allow_local: bool,
) -> Option<Vec<String>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let url = validate_url(&url, allow_local).await.ok()?;

    let mut headers = extra_headers.clone();
    if let Some(key) = api_key {
        if backend == "messages" {
            headers.insert("x-api-key".into(), key.into());
        } else {
            headers.insert("Authorization".into(), format!("Bearer {key}"));
        }
    }

    let text = http_get_text(&url, Duration::from_secs(10)).await.ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("data")?
        .as_array()?
        .iter()
        .filter_map(|m| m.get("id")?.as_str().map(|s| s.to_string()))
        .collect::<Vec<_>>()
        .into()
}

pub async fn test_provider(id: &str) -> Result<(bool, Option<String>)> {
    let provider = get_provider(id)?.ok_or_else(|| anyhow::anyhow!("provider '{id}' not found"))?;
    let api_key = resolve_api_key(&provider);
    let base_url = provider.base_url.trim_end_matches('/').to_string();
    let backend = provider
        .api_backend
        .as_deref()
        .unwrap_or("chat_completions");

    let mut headers = provider.extra_headers.clone().unwrap_or_default();
    if let Some(key) = &api_key {
        if backend == "messages" {
            headers.insert("x-api-key".into(), key.clone());
        } else {
            headers.insert("Authorization".into(), format!("Bearer {key}"));
        }
    }

    let allow_local = provider.id == "ollama"
        || provider.id == "lmstudio"
        || provider.base_url.contains("localhost")
        || provider.base_url.contains("127.0.0.1");

    if let Some(models) = fetch_model_list(
        &base_url,
        api_key.as_deref(),
        backend,
        &headers,
        allow_local,
    )
    .await
    {
        if !models.is_empty() {
            return Ok((true, None));
        }
    }

    if backend == "chat_completions" {
        let url = validate_url(&format!("{base_url}/chat/completions"), allow_local).await?;
        let body = serde_json::json!({
            "model": provider.model,
            "messages": [{"role": "system", "content": "ping"}],
            "max_tokens": 1,
        });
        let (status, text) = http_post_json(&url, headers, body, Duration::from_secs(10)).await?;
        if status == 200 {
            Ok((true, None))
        } else {
            Ok((false, Some(format!("HTTP {status}: {text}"))))
        }
    } else if backend == "responses" {
        let url = validate_url(&format!("{base_url}/responses"), allow_local).await?;
        let body = serde_json::json!({
            "model": provider.model,
            "input": "ping",
            "max_output_tokens": 1,
        });
        let (status, text) = http_post_json(&url, headers, body, Duration::from_secs(10)).await?;
        if status == 200 {
            Ok((true, None))
        } else {
            Ok((false, Some(format!("HTTP {status}: {text}"))))
        }
    } else if backend == "messages" {
        let url = validate_url(&format!("{base_url}/messages"), allow_local).await?;
        let body = serde_json::json!({
            "model": provider.model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
        });
        let (status, text) = http_post_json(&url, headers, body, Duration::from_secs(10)).await?;
        if status == 200 {
            Ok((true, None))
        } else {
            Ok((false, Some(format!("HTTP {status}: {text}"))))
        }
    } else {
        Ok((
            false,
            Some("provider did not respond to models list".into()),
        ))
    }
}

pub fn sanitize_provider_id(id: &str) -> String {
    id.to_ascii_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '_' && c != '-', "-")
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_provider_id() {
        assert_eq!(sanitize_provider_id("OpenAI"), "openai");
        assert_eq!(sanitize_provider_id("my provider!"), "my-provider");
        assert_eq!(sanitize_provider_id("-llama-cpp-"), "llama-cpp");
    }

    #[test]
    fn test_env_var_name() {
        assert_eq!(env_var_name("openai"), "OMGB_OPENAI_API_KEY");
        assert_eq!(env_var_name("llama-cpp"), "OMGB_LLAMA_CPP_API_KEY");
    }

    #[test]
    fn test_resolve_api_key_prefers_env() {
        let provider = ProviderConfig {
            id: "test".into(),
            name: "Test".into(),
            model: "gpt".into(),
            base_url: "http://localhost/v1".into(),
            api_backend: None,
            env_key: Some(vec!["OMGB_TEST_API_KEY".into()]),
            extra_headers: None,
            context_window: None,
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        };
        std::env::set_var("OMGB_TEST_API_KEY", "secret");
        assert_eq!(resolve_api_key(&provider), Some("secret".into()));
        std::env::remove_var("OMGB_TEST_API_KEY");
    }
}
