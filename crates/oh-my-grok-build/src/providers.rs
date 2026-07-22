//! BYOK and local-model provider management for `omgb`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::args::{AddProviderArgs, DiscoverArgs};
use crate::net::{http_get_text, http_post_json, is_url_host_private, validate_url};
use url::Url;

pub mod catalog;

const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434/v1";
const DEFAULT_LMSTUDIO_URL: &str = "http://localhost:1234/v1";
const DEFAULT_VLLM_URL: &str = "http://localhost:8000/v1";
const DEFAULT_LLAMA_CPP_URL: &str = "http://localhost:8080/v1";
const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

#[derive(Debug, Clone)]
pub struct ModelListEntry {
    pub id: String,
    pub context_window: Option<u64>,
}

pub fn omg_dir() -> Result<PathBuf> {
    if let Ok(v) = std::env::var("OMGB_HOME") {
        return Ok(PathBuf::from(v));
    }
    dirs::home_dir()
        .map(|h| h.join(".omgb"))
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory; set OMGB_HOME"))
}

fn omg_config_path() -> Result<PathBuf> {
    Ok(omg_dir()?.join("config.json"))
}

fn omg_env_path() -> Result<PathBuf> {
    Ok(omg_dir()?.join(".env"))
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
    pub auto_compact_threshold_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u64>,
}

pub fn load_omg_config() -> Result<OmgConfig> {
    let path = omg_config_path()?;
    if !path.exists() {
        return Ok(OmgConfig::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

pub fn save_omg_config(config: &OmgConfig) -> Result<()> {
    let dir = omg_dir()?;
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("config.json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(config)?)?;
    let path = omg_config_path()?;
    std::fs::rename(&tmp, &path)?;
    restrict_env_file_permissions(&path)?;
    Ok(())
}

pub(crate) fn load_env_file() -> Result<HashMap<String, String>> {
    let path = omg_env_path()?;
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    Ok(parse_env_entries(&raw).into_iter().collect())
}

/// Returns the set of `*_API_KEY` environment variable names that should be
/// loaded into the process environment at startup. Only keys referenced by a
/// configured provider, connector, or known catalog template are loaded, and
/// only when a non-empty value is present in `~/.omgb/.env`. This limits the
/// secrets exposed to child processes while still letting catalog-based MoE
/// routing discover keys before a provider is persisted to config.
pub(crate) fn env_keys_to_load() -> HashSet<String> {
    let mut keys = HashSet::new();
    let mut collect = |p: &ProviderConfig| {
        for k in valid_env_keys(p) {
            keys.insert(k);
        }
    };

    if let Ok(cfg) = load_omg_config() {
        for p in cfg.providers.values() {
            collect(p);
        }
    }
    for t in catalog::TEMPLATES {
        collect(&t.to_provider_config());
    }
    if let Ok(dir) = omg_dir() {
        let connectors_path = dir.join("connectors.json");
        if let Ok(raw) = std::fs::read_to_string(&connectors_path)
            && let Ok(serde_json::Value::Object(registry)) =
                serde_json::from_str::<serde_json::Value>(&raw)
            && let Some(serde_json::Value::Object(connectors)) = registry.get("connectors")
        {
            for (name, value) in connectors {
                keys.insert(env_var_name(name));
                if let Some(secret) = value
                    .get("secret_env_key")
                    .and_then(|v| v.as_str())
                    .filter(|k| is_valid_env_key(k))
                {
                    keys.insert(secret.to_string());
                }
            }
        }
    }

    let referenced: HashSet<String> = keys.clone();
    if let Ok(path) = omg_env_path()
        && let Ok(raw) = std::fs::read_to_string(&path)
    {
        for (k, _) in parse_env_entries(&raw) {
            if is_valid_env_key(&k) && referenced.contains(&k) {
                keys.insert(k);
            }
        }
    }
    keys
}

fn parse_env_entries(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let line = trimmed
            .strip_prefix("export ")
            .unwrap_or(trimmed)
            .trim_start();
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_string();
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            out.push((key, parse_env_value(v.trim())));
        }
    }
    out
}

fn parse_env_value(raw: &str) -> String {
    if let Some(s) = raw.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '"' {
                break;
            }
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(match next {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '\\' => '\\',
                        '"' => '"',
                        '\'' => '\'',
                        other => other,
                    });
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if let Some(s) = raw.strip_prefix('\'') {
        s.split('\'').next().unwrap_or(s).to_string()
    } else {
        raw.trim().to_string()
    }
}

fn format_env_value(value: &str) -> String {
    let needs_quote = value.is_empty()
        || value.chars().any(|c| {
            c.is_whitespace() || c == '=' || c == '"' || c == '\\' || c == '#' || c == '\''
        });
    if !needs_quote {
        return value.to_string();
    }
    let mut out = String::from('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

pub(crate) fn write_file_atomic(path: &std::path::Path, content: impl AsRef<[u8]>) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, content.as_ref())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn write_env_entries(entries: &[(String, String)]) -> Result<()> {
    let path = omg_env_path()?;
    let mut content = String::new();
    for (k, v) in entries {
        content.push_str(&format!("{}={}\n", k, format_env_value(v)));
    }
    write_file_atomic(&path, content)?;
    restrict_env_file_permissions(&path)
}

pub(crate) fn restrict_env_file_permissions(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").map_err(|_| {
            anyhow::anyhow!("USERNAME env var not set; cannot restrict file permissions")
        })?;
        let status = std::process::Command::new("icacls")
            .arg(path)
            .arg("/inheritance:r")
            .status()?;
        if !status.success() {
            bail!("icacls /inheritance:r failed for {}", path.display());
        }
        let status = std::process::Command::new("icacls")
            .arg(path)
            .arg("/grant:r")
            .arg(format!("{user}:F"))
            .status()?;
        if !status.success() {
            bail!("icacls /grant:r failed for {}", path.display());
        }
    }
    Ok(())
}

pub(crate) fn env_var_name(provider_id: &str) -> String {
    format!(
        "OMGB_{}_API_KEY",
        provider_id.replace('-', "_").to_uppercase()
    )
}

fn provider_env_keys(provider_id: &str, canonical: Option<&str>) -> Option<Vec<String>> {
    let storage = env_var_name(provider_id);
    let mut keys = vec![storage];
    if let Some(c) = canonical {
        let c = c.to_string();
        if !keys.contains(&c) {
            keys.push(c);
        }
    }
    Some(keys)
}

pub(crate) fn is_valid_env_key(key: &str) -> bool {
    !key.is_empty()
        && key.ends_with("_API_KEY")
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn is_url_host_loopback(raw: &str) -> bool {
    crate::net::is_url_host_loopback(raw)
}

fn is_ollama_url(raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    url.port() == Some(11434) && crate::net::is_url_host_loopback(raw)
}

fn api_key_target_var(provider_id: &str, env_keys: Option<&[String]>) -> String {
    env_keys
        .and_then(|keys| keys.iter().find(|k| is_valid_env_key(k)).cloned())
        .unwrap_or_else(|| env_var_name(provider_id))
}

pub fn write_api_key(provider_id: &str, env_keys: Option<&[String]>, key: &str) -> Result<String> {
    let target = api_key_target_var(provider_id, env_keys);
    if !is_valid_env_key(&target) {
        bail!("refusing to write API key for invalid env var {target}");
    }
    let legacy = env_var_name(provider_id);
    let path = omg_env_path()?;
    let mut entries = if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        parse_env_entries(&raw)
    } else {
        Vec::new()
    };
    entries.retain(|(k, _)| *k != target && (target == legacy || *k != legacy));
    entries.push((target.clone(), key.to_string()));
    write_env_entries(&entries)?;
    Ok(target)
}

fn is_env_key_referenced(
    target: &str,
    exclude_provider: Option<&str>,
    exclude_connector: Option<&str>,
) -> Result<bool> {
    if let Ok(cfg) = load_omg_config() {
        for (id, p) in &cfg.providers {
            if exclude_provider == Some(id.as_str()) {
                continue;
            }
            if p.env_key
                .as_ref()
                .is_some_and(|keys| keys.iter().any(|k| k == target))
            {
                return Ok(true);
            }
        }
    }
    let connectors_path = omg_dir()?.join("connectors.json");
    if let Ok(raw) = std::fs::read_to_string(&connectors_path)
        && let Ok(serde_json::Value::Object(registry)) =
            serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(serde_json::Value::Object(connectors)) = registry.get("connectors")
    {
        for (name, value) in connectors {
            if exclude_connector == Some(name.as_str()) {
                continue;
            }
            if env_var_name(name) == target {
                return Ok(true);
            }
            if value
                .get("secret_env_key")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == target)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn remove_api_key(name: &str, is_connector: bool, env_keys: Option<&[String]>) -> Result<()> {
    let path = omg_env_path()?;
    if !path.exists() {
        return Ok(());
    }
    let storage = env_var_name(name);
    let mut keys: Vec<String> = env_keys
        .map(|v| v.iter().filter(|k| is_valid_env_key(k)).cloned().collect())
        .unwrap_or_default();
    if !keys.contains(&storage) {
        keys.insert(0, storage);
    }
    keys.dedup();
    let (exclude_provider, exclude_connector) = if is_connector {
        (None, Some(name))
    } else {
        (Some(name), None)
    };
    let entries = parse_env_entries(&std::fs::read_to_string(&path)?);
    let mut retained = Vec::new();
    for (k, v) in entries {
        if !keys.contains(&k) || is_env_key_referenced(&k, exclude_provider, exclude_connector)? {
            retained.push((k, v));
        }
    }
    write_env_entries(&retained)?;
    Ok(())
}

fn valid_env_keys(provider: &ProviderConfig) -> Vec<String> {
    let mut keys: Vec<_> = provider
        .env_key
        .as_ref()
        .map(|keys| {
            keys.iter()
                .filter(|k| is_valid_env_key(k))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let storage = env_var_name(&provider.id);
    if !keys.contains(&storage) {
        keys.push(storage);
    }
    if keys.is_empty() {
        vec![env_var_name(&provider.id)]
    } else {
        keys
    }
}

fn resolve_api_key_with_maps(
    provider: &ProviderConfig,
    env: &HashMap<String, String>,
    dotenv: &HashMap<String, String>,
) -> Option<String> {
    let keys = valid_env_keys(provider);
    for k in &keys {
        if let Some(v) = env.get(k)
            && !v.is_empty()
        {
            return Some(v.clone());
        }
    }
    for k in &keys {
        if let Some(v) = dotenv.get(k)
            && !v.is_empty()
        {
            return Some(v.clone());
        }
    }
    None
}

pub fn resolve_api_key(provider: &ProviderConfig) -> Result<Option<String>> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let dotenv = load_env_file()?;
    Ok(resolve_api_key_with_maps(provider, &env, &dotenv))
}

pub fn resolve_env_key(key: &str) -> Result<Option<String>> {
    if !is_valid_env_key(key) {
        return Ok(None);
    }
    if let Ok(v) = std::env::var(key)
        && !v.is_empty()
    {
        return Ok(Some(v));
    }
    let dotenv = load_env_file()?;
    Ok(dotenv.get(key).filter(|v| !v.is_empty()).cloned())
}

pub fn list_providers() -> Result<Vec<ProviderConfig>> {
    let cfg = load_omg_config()?;
    Ok(cfg.providers.values().cloned().collect())
}

pub fn get_provider(id: &str) -> Result<Option<ProviderConfig>> {
    let cfg = load_omg_config()?;
    Ok(cfg.providers.get(id).cloned())
}

/// If `id` is already in `~/.omgb/config.json`, return it. Otherwise try to
/// materialise a known built-in or catalog template, persist it, and sync it to
/// `~/.grok/config.toml` so upstream model resolution can use `omgb-{id}`.
pub fn ensure_provider_configured(id: &str) -> Result<ProviderConfig> {
    let id = sanitize_provider_id(id);
    let mut cfg = load_omg_config()?;
    if let Some(p) = cfg.providers.get(&id).cloned() {
        return Ok(p);
    }
    let provider = provider_template(&id).ok_or_else(|| {
        anyhow::anyhow!("provider '{id}' is not configured and has no known template")
    })?;
    if provider.model.trim().is_empty() {
        bail!("provider '{id}' has no configured model; pass --model or discover local models");
    }
    cfg.providers.insert(id.clone(), provider.clone());
    save_omg_config(&cfg)?;
    sync_provider_to_grok_config(&provider)?;
    Ok(provider)
}

pub fn remove_provider(id: &str) -> Result<()> {
    let mut cfg = load_omg_config()?;
    let provider = cfg.providers.remove(id);
    if cfg.default_model.as_deref() == Some(&format!("omgb-{id}")) {
        cfg.default_model = None;
    }
    save_omg_config(&cfg)?;
    remove_api_key(
        id,
        false,
        provider.as_ref().and_then(|p| p.env_key.as_deref()),
    )?;
    remove_provider_from_grok_config(id)?;
    Ok(())
}

pub(crate) fn provider_template(id: &str) -> Option<ProviderConfig> {
    match id {
        "openai" => Some(ProviderConfig {
            id: "openai".into(),
            name: "OpenAI".into(),
            model: "gpt-4o".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_backend: Some("chat_completions".into()),
            env_key: provider_env_keys(id, Some("OPENAI_API_KEY")),
            extra_headers: None,
            context_window: Some(128_000),
            auto_compact_threshold_percent: Some(80),
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
            env_key: provider_env_keys(id, Some("ANTHROPIC_API_KEY")),
            extra_headers: Some(HashMap::from([(
                "anthropic-version".into(),
                "2023-06-01".into(),
            )])),
            context_window: Some(200_000),
            auto_compact_threshold_percent: Some(80),
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
            env_key: provider_env_keys(id, Some("XAI_API_KEY")),
            extra_headers: None,
            context_window: Some(500_000),
            auto_compact_threshold_percent: Some(80),
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
            env_key: provider_env_keys(id, Some("OPENROUTER_API_KEY")),
            extra_headers: Some(HashMap::from([
                ("HTTP-Referer".into(), "https://oh-my-grok.build".into()),
                ("X-Title".into(), "oh-my-grok-build".into()),
            ])),
            context_window: Some(200_000),
            auto_compact_threshold_percent: Some(80),
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
            env_key: provider_env_keys(id, None),
            extra_headers: None,
            context_window: Some(128_000),
            auto_compact_threshold_percent: Some(80),
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
            env_key: provider_env_keys(id, Some("LMSTUDIO_API_KEY")),
            extra_headers: None,
            context_window: Some(128_000),
            auto_compact_threshold_percent: Some(80),
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
            env_key: provider_env_keys(id, Some("OMGB_VLLM_API_KEY")),
            extra_headers: None,
            context_window: Some(128_000),
            auto_compact_threshold_percent: Some(80),
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
            env_key: provider_env_keys(id, Some("OMGB_LLAMA_CPP_API_KEY")),
            extra_headers: None,
            context_window: Some(128_000),
            auto_compact_threshold_percent: Some(80),
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
            env_key: provider_env_keys(id, Some("OMGB_TABBY_API_KEY")),
            extra_headers: None,
            context_window: Some(128_000),
            auto_compact_threshold_percent: Some(80),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }),
        _ => catalog::provider_template(id),
    }
}

pub async fn add_provider(args: &AddProviderArgs) -> Result<ProviderConfig> {
    let id = sanitize_provider_id(&args.id);
    if id.is_empty() {
        bail!("provider id is required");
    }

    let mut cfg = load_omg_config()?;
    let mut provider = if let Some(t) = &args.template {
        provider_template(t).ok_or_else(|| anyhow::anyhow!("unknown template '{t}'"))?
    } else if let Some(p) = provider_template(&id) {
        p
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
            api_backend: Some(
                args.backend
                    .as_ref()
                    .map(|b| b.as_str().into())
                    .unwrap_or_else(|| "chat_completions".into()),
            ),
            env_key: None,
            extra_headers: None,
            context_window: None,
            auto_compact_threshold_percent: Some(80),
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
        if !is_valid_env_key(env_key) {
            bail!(
                "--env-key must end with _API_KEY and contain only uppercase A-Z, 0-9, and underscores"
            );
        }
        provider.env_key = Some(vec![env_key.clone()]);
    }
    if let Some(cw) = args.context_window {
        provider.context_window = Some(cw);
    }
    if let Some(th) = args.auto_compact_threshold_percent {
        provider.auto_compact_threshold_percent = Some(th.clamp(0, 100));
    }
    provider.id = id.clone();
    if provider.model.trim().is_empty() {
        bail!("--model is required for provider '{id}'");
    }
    if let Some(backend) = &args.backend {
        provider.api_backend = Some(backend.as_str().into());
    }

    // Always prepend the provider-specific storage key so writes never clobber a shared canonical key.
    let storage = env_var_name(&id);
    let mut env_keys = provider.env_key.take().unwrap_or_default();
    if !env_keys.iter().any(|k| k == &storage) {
        env_keys.insert(0, storage);
    }
    provider.env_key = Some(env_keys);

    // API keys are only accepted via the OMGB_API_KEY environment variable so they
    // never appear in shell history or process listings. Do not persist the key
    // until the provider config has been fully validated and saved.
    let api_key = std::env::var("OMGB_API_KEY").ok().filter(|s| !s.is_empty());

    if provider.context_window.is_none() {
        let api_key_for_fetch = if let Some(ref k) = api_key {
            Some(k.clone())
        } else {
            resolve_api_key(&provider)?
        };
        let backend = provider
            .api_backend
            .as_deref()
            .unwrap_or("chat_completions");
        let extra = provider.extra_headers.clone().unwrap_or_default();
        let allow_local = provider.id.starts_with("ollama")
            || provider.id.starts_with("lmstudio")
            || provider.id.starts_with("local-")
            || is_url_host_loopback(&provider.base_url);
        let allow_private = is_url_host_private(&provider.base_url).await;
        // Reject insecure public HTTP before the provider is saved.
        let _ = validate_url(&provider.base_url, allow_local, allow_private).await?;
        let is_ollama = provider.id == "ollama" || is_ollama_url(&provider.base_url);
        let cw = fetch_model_context_window(
            &provider.base_url,
            api_key_for_fetch.as_deref(),
            backend,
            &extra,
            allow_local,
            allow_private,
            is_ollama,
            &provider.model,
        )
        .await;
        provider.context_window = cw
            .or_else(|| fallback_context_window(&provider.model))
            .or(Some(DEFAULT_CONTEXT_WINDOW));
    }
    if provider.auto_compact_threshold_percent.is_none() {
        provider.auto_compact_threshold_percent = Some(80);
    }

    if args.default || cfg.default_model.is_none() {
        cfg.default_model = Some(format!("omgb-{id}"));
    }
    cfg.providers.insert(id.clone(), provider.clone());
    save_omg_config(&cfg)?;
    sync_provider_to_grok_config(&provider)?;

    // Persist the API key only after the provider config has been saved. This
    // avoids leaving orphaned secrets in ~/.omgb/.env if validation fails.
    if let Some(key) = api_key {
        write_api_key(&id, provider.env_key.as_deref(), &key)?;
    }

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

fn apply_provider_to_grok_table(
    provider: &ProviderConfig,
    gcfg: &mut toml::map::Map<String, toml::Value>,
) {
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
        let valid: Vec<_> = keys
            .iter()
            .filter(|k| is_valid_env_key(k))
            .cloned()
            .collect();
        if !valid.is_empty() {
            if valid.len() == 1 {
                section.insert("env_key".into(), toml::Value::String(valid[0].clone()));
            } else {
                section.insert(
                    "env_key".into(),
                    toml::Value::Array(valid.into_iter().map(toml::Value::String).collect()),
                );
            }
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
    if let Some(th) = provider.auto_compact_threshold_percent {
        section.insert(
            "auto_compact_threshold_percent".into(),
            toml::Value::Integer(th as i64),
        );
    }

    let model = gcfg
        .entry("model")
        .or_insert(toml::Value::Table(toml::map::Map::new()));
    if let toml::Value::Table(m) = model {
        m.insert(model_key, toml::Value::Table(section));
    }
}

fn set_grok_default_if_unset(gcfg: &mut toml::map::Map<String, toml::Value>, model_key: &str) {
    let models = gcfg
        .entry("models")
        .or_insert(toml::Value::Table(toml::map::Map::new()));
    if let toml::Value::Table(m) = models
        && !m.contains_key("default")
    {
        m.insert("default".into(), toml::Value::String(model_key.into()));
    }
}

fn sync_provider_to_grok_config(provider: &ProviderConfig) -> Result<()> {
    let mut gcfg = load_grok_config_table()?;
    apply_provider_to_grok_table(provider, &mut gcfg);
    set_grok_default_if_unset(&mut gcfg, &format!("omgb-{}", provider.id));
    save_grok_config_table(&gcfg)?;
    Ok(())
}

fn remove_provider_from_grok_config(id: &str) -> Result<()> {
    let mut gcfg = load_grok_config_table()?;
    let model_key = format!("omgb-{id}");
    if let Some(toml::Value::Table(m)) = gcfg.get_mut("model") {
        m.remove(&model_key);
    }
    let remaining: Vec<String> = gcfg
        .get("model")
        .and_then(|v| v.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    if let Some(toml::Value::Table(m)) = gcfg.get_mut("models")
        && m.get("default").and_then(|v| v.as_str()) == Some(&model_key)
    {
        if let Some(first) = remaining.first() {
            m.insert("default".into(), toml::Value::String(first.clone()));
        } else {
            m.remove("default");
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
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("grok config path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let raw = toml::to_string_pretty(&toml::Value::Table(table.clone()))?;
    let tmp = parent.join(format!("config.toml.tmp.{}", std::process::id()));
    std::fs::write(&tmp, raw)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

async fn discover_one(base_url: &str, name: &str) -> Option<(String, String, Vec<ModelListEntry>)> {
    let models = fetch_model_list(
        base_url,
        None,
        "chat_completions",
        &HashMap::new(),
        true,
        true,
    )
    .await?;
    if models.is_empty() {
        return None;
    }
    Some((name.into(), base_url.into(), models))
}

pub async fn discover_local_models(
    args: &DiscoverArgs,
) -> Result<Vec<(String, String, Vec<ModelListEntry>)>> {
    let ollama = args.ollama_url.as_deref().unwrap_or(DEFAULT_OLLAMA_URL);
    let lmstudio = args.lmstudio_url.as_deref().unwrap_or(DEFAULT_LMSTUDIO_URL);

    let (ollama, lmstudio, vllm, llama_cpp) = tokio::join!(
        discover_one(ollama, "ollama"),
        discover_one(lmstudio, "lmstudio"),
        discover_one(DEFAULT_VLLM_URL, "vllm"),
        discover_one(DEFAULT_LLAMA_CPP_URL, "llama-cpp"),
    );

    Ok([ollama, lmstudio, vllm, llama_cpp]
        .into_iter()
        .flatten()
        .collect())
}

pub fn add_discovered_providers(
    discovered: &[(String, String, Vec<ModelListEntry>)],
) -> Result<()> {
    let mut cfg = load_omg_config()?;
    let mut first_id: Option<String> = None;
    for (provider, base_url, models) in discovered {
        for model in models {
            let model_id = sanitize_provider_id(&model.id);
            let id = format!("{provider}-{model_id}");
            let config = ProviderConfig {
                id: id.clone(),
                name: format!("{provider} {model_id} (local)"),
                model: model.id.clone(),
                base_url: base_url.into(),
                api_backend: Some("chat_completions".into()),
                env_key: provider_env_keys(&id, None),
                extra_headers: None,
                context_window: Some(model.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW)),
                auto_compact_threshold_percent: Some(80),
                temperature: None,
                top_p: None,
                max_completion_tokens: None,
            };
            cfg.providers.insert(id.clone(), config);
            if first_id.is_none() {
                first_id = Some(id);
            }
        }
    }
    if cfg.default_model.is_none()
        && let Some(id) = first_id
    {
        cfg.default_model = Some(format!("omgb-{id}"));
    }
    save_omg_config(&cfg)?;

    let mut gcfg = load_grok_config_table()?;
    for p in cfg.providers.values() {
        apply_provider_to_grok_table(p, &mut gcfg);
    }
    if let Some(default) = cfg.default_model.as_deref() {
        set_grok_default_if_unset(&mut gcfg, default);
    }
    save_grok_config_table(&gcfg)?;
    Ok(())
}

fn extract_context_window(value: &serde_json::Value) -> Option<u64> {
    fn is_context_key(k: &str) -> bool {
        k == "context_length"
            || k == "context_window"
            || k.ends_with(".context_length")
            || k.ends_with(".context_window")
    }

    value
        .get("context_length")
        .or_else(|| value.get("context_window"))
        .or_else(|| value.get("max_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| {
            value.get("model_info").and_then(|mi| {
                mi.as_object()
                    .and_then(|obj| obj.iter().find(|(k, _)| is_context_key(k)).map(|(_, v)| v))
                    .and_then(|v| v.as_u64())
            })
        })
}

async fn fetch_model_list(
    base_url: &str,
    api_key: Option<&str>,
    backend: &str,
    extra_headers: &HashMap<String, String>,
    allow_local: bool,
    allow_private: bool,
) -> Option<Vec<ModelListEntry>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let url = validate_url(&url, allow_local, allow_private).await.ok()?;

    let mut headers = extra_headers.clone();
    if let Some(key) = api_key {
        if backend == "messages" {
            headers.insert("x-api-key".into(), key.into());
        } else {
            headers.insert("Authorization".into(), format!("Bearer {key}"));
        }
    }

    let text = http_get_text(&url, Some(&headers), Duration::from_secs(10))
        .await
        .ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(
        json.get("data")?
            .as_array()?
            .iter()
            .filter_map(|m| {
                let id = m.get("id")?.as_str()?.to_string();
                let context_window = extract_context_window(m);
                Some(ModelListEntry { id, context_window })
            })
            .collect(),
    )
}

/// Convert an OpenAI-compatible Ollama base URL (e.g. `http://host:11434/v1`)
/// into the corresponding `/api/show` endpoint. The trailing `/v1` path segment
/// (with or without a trailing slash) is removed; any other path prefix is
/// preserved.
fn ollama_show_url(base_url: &str) -> Option<String> {
    let mut url = Url::parse(base_url).ok()?;
    let mut segs: Vec<String> = url
        .path_segments()?
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if segs.last().is_some_and(|s| s == "v1") {
        segs.pop();
    }
    segs.push("api".into());
    segs.push("show".into());
    let mut ps = url.path_segments_mut().ok()?;
    ps.clear();
    for s in &segs {
        ps.push(s);
    }
    drop(ps);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

async fn fetch_model_context_window(
    base_url: &str,
    api_key: Option<&str>,
    backend: &str,
    extra_headers: &HashMap<String, String>,
    allow_local: bool,
    allow_private: bool,
    is_ollama: bool,
    model: &str,
) -> Option<u64> {
    let models = fetch_model_list(
        base_url,
        api_key,
        backend,
        extra_headers,
        allow_local,
        allow_private,
    )
    .await?;
    let entry = models.iter().find(|m| m.id == model);
    if let Some(cw) = entry.and_then(|m| m.context_window) {
        return Some(cw);
    }
    if is_ollama {
        let show_url = ollama_show_url(base_url)?;
        let vurl = validate_url(&show_url, allow_local, allow_private)
            .await
            .ok()?;
        let mut headers = extra_headers.clone();
        if let Some(key) = api_key {
            headers.insert("Authorization".into(), format!("Bearer {key}"));
        }
        let body = serde_json::json!({"name": model});
        let (_, text) = http_post_json(&vurl, &headers, body, Duration::from_secs(10))
            .await
            .ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        return extract_context_window(&json);
    }
    None
}

fn fallback_context_window(model: &str) -> Option<u64> {
    let lower = model.to_ascii_lowercase();
    if lower.contains("gpt-4o") || lower.contains("gpt-4-turbo") {
        Some(128_000)
    } else if lower.contains("claude-3") {
        Some(200_000)
    } else if lower.contains("grok-4.5") || lower.contains("grok-4") {
        Some(500_000)
    } else if lower.contains("grok-2") {
        Some(131_072)
    } else if lower.contains("llama-3")
        || lower.contains("codellama")
        || lower.contains("qwen2")
        || lower.contains("mistral")
        || lower.contains("mixtral")
        || lower.contains("phi-3")
        || lower.contains("phi3")
    {
        Some(128_000)
    } else {
        None
    }
}

pub async fn test_provider(id: &str) -> Result<(bool, Option<String>)> {
    let provider = get_provider(id)?.ok_or_else(|| anyhow::anyhow!("provider '{id}' not found"))?;
    let api_key = resolve_api_key(&provider)?;
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

    let allow_local = provider.id.starts_with("ollama")
        || provider.id.starts_with("lmstudio")
        || provider.id.starts_with("local-")
        || is_url_host_loopback(&provider.base_url);
    let allow_private = is_url_host_private(&provider.base_url).await;

    if let Some(models) = fetch_model_list(
        &base_url,
        api_key.as_deref(),
        backend,
        &headers,
        allow_local,
        allow_private,
    )
    .await
        && !models.is_empty()
    {
        return Ok((true, None));
    }

    if backend == "chat_completions" {
        let url = validate_url(
            &format!("{base_url}/chat/completions"),
            allow_local,
            allow_private,
        )
        .await?;
        let body = serde_json::json!({
            "model": provider.model,
            "messages": [{"role": "system", "content": "ping"}],
            "max_tokens": 1,
        });
        let (status, text) = http_post_json(&url, &headers, body, Duration::from_secs(10)).await?;
        if status == 200 {
            Ok((true, None))
        } else {
            Ok((false, Some(format!("HTTP {status}: {text}"))))
        }
    } else if backend == "responses" {
        let url =
            validate_url(&format!("{base_url}/responses"), allow_local, allow_private).await?;
        let body = serde_json::json!({
            "model": provider.model,
            "input": "ping",
            "max_output_tokens": 1,
        });
        let (status, text) = http_post_json(&url, &headers, body, Duration::from_secs(10)).await?;
        if status == 200 {
            Ok((true, None))
        } else {
            Ok((false, Some(format!("HTTP {status}: {text}"))))
        }
    } else if backend == "messages" {
        let url = validate_url(&format!("{base_url}/messages"), allow_local, allow_private).await?;
        let body = serde_json::json!({
            "model": provider.model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
        });
        let (status, text) = http_post_json(&url, &headers, body, Duration::from_secs(10)).await?;
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
        .replace(
            |c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-',
            "-",
        )
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
        assert_eq!(sanitize_provider_id("café"), "caf");
        assert!(sanitize_provider_id("---").is_empty());
    }

    #[test]
    fn test_is_valid_env_key() {
        assert!(is_valid_env_key("OMGB_OPENAI_API_KEY"));
        assert!(!is_valid_env_key("PATH"));
        assert!(!is_valid_env_key("OMGB_CAFÉ_API_KEY"));
        assert!(!is_valid_env_key(""));
    }

    #[test]
    fn test_write_api_key_rejects_invalid_env_var() {
        assert!(write_api_key("café", None, "secret").is_err());
    }

    #[test]
    fn test_env_var_name() {
        assert_eq!(env_var_name("openai"), "OMGB_OPENAI_API_KEY");
        assert_eq!(env_var_name("llama-cpp"), "OMGB_LLAMA_CPP_API_KEY");
    }

    #[test]
    fn test_resolve_api_key_prefers_env_over_dotenv() {
        let provider = ProviderConfig {
            id: "test".into(),
            name: "Test".into(),
            model: "gpt".into(),
            base_url: "http://localhost/v1".into(),
            api_backend: None,
            env_key: Some(vec!["OMGB_TEST_API_KEY".into()]),
            extra_headers: None,
            context_window: None,
            auto_compact_threshold_percent: Some(80),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        };
        let mut env = HashMap::new();
        env.insert("OMGB_TEST_API_KEY".into(), "from-env".into());
        let mut dotenv = HashMap::new();
        dotenv.insert("OMGB_TEST_API_KEY".into(), "from-dotenv".into());
        assert_eq!(
            resolve_api_key_with_maps(&provider, &env, &dotenv),
            Some("from-env".into())
        );
        let empty_env = HashMap::new();
        assert_eq!(
            resolve_api_key_with_maps(&provider, &empty_env, &dotenv),
            Some("from-dotenv".into())
        );
        let empty_dotenv = HashMap::new();
        assert_eq!(
            resolve_api_key_with_maps(&provider, &env, &empty_dotenv),
            Some("from-env".into())
        );
    }

    #[test]
    fn test_ensure_provider_configured_rejects_empty_model() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let tmp =
            std::env::temp_dir().join(format!("omgb-providers-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe { std::env::set_var("OMGB_HOME", tmp.as_os_str()) };
        let err = ensure_provider_configured("vllm").unwrap_err();
        assert!(err.to_string().contains("no configured model"));
        unsafe { std::env::remove_var("OMGB_HOME") };
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
