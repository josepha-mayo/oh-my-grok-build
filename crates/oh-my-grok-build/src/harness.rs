//! Cross-harness connector management for `omgb`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::args::HarnessType;
use crate::taste::taste_preamble;

fn registry_path() -> PathBuf {
    crate::providers::omg_dir().join("connectors.json")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectorRegistry {
    #[serde(default)]
    pub connectors: HashMap<String, ConnectorConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfig {
    pub name: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_env_key: Option<String>,
}

fn load_registry() -> Result<ConnectorRegistry> {
    let path = registry_path();
    if !path.exists() {
        return Ok(ConnectorRegistry::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn save_registry(registry: &ConnectorRegistry) -> Result<()> {
    let path = registry_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(registry)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn default_command(r#type: &str) -> Option<String> {
    match r#type {
        "codex" => Some("codex exec --json {prompt}".into()),
        "claude" => Some("claude -p {prompt}".into()),
        "opencode" => Some("opencode run {prompt}".into()),
        "hermes" => Some("hermes run {prompt}".into()),
        "pi" => Some("pi run {prompt}".into()),
        "omp" => Some("omp run {prompt}".into()),
        _ => None,
    }
}

pub fn add_connector(
    name: String,
    r#type: HarnessType,
    command: Option<String>,
    url: Option<String>,
    cwd: Option<PathBuf>,
    secret: Option<String>,
) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        bail!("invalid connector name");
    }
    let type_str = r#type.as_str().to_string();
    let command = command.or_else(|| default_command(&type_str));
    let mut registry = load_registry()?;

    let secret_env_key = secret
        .as_ref()
        .map(|s| crate::providers::write_api_key(&name, None, s))
        .transpose()?;

    registry.connectors.insert(
        name.clone(),
        ConnectorConfig {
            name,
            r#type: type_str,
            command,
            url,
            cwd,
            secret_env_key,
        },
    );
    save_registry(&registry)
}

pub fn list_connectors() -> Result<Vec<ConnectorConfig>> {
    Ok(load_registry()?.connectors.values().cloned().collect())
}

pub fn remove_connector(name: &str) -> Result<()> {
    let mut registry = load_registry()?;
    let cfg = registry.connectors.remove(name);
    save_registry(&registry)?;
    if let Some(cfg) = cfg {
        if let Some(ref key) = cfg.secret_env_key {
            let keys = vec![key.clone()];
            let _ = crate::providers::remove_api_key(name, Some(keys.as_slice()));
        } else {
            let _ = crate::providers::remove_api_key(name, None);
        }
    }
    Ok(())
}

pub async fn run_connector(name: &str, prompt: &str) -> Result<()> {
    let registry = load_registry()?;
    let cfg = registry
        .connectors
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("connector '{name}' not found"))?;

    if let Some(url) = &cfg.url {
        return run_http_connector(&cfg, url, prompt).await;
    }

    let command = cfg
        .command
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("connector has no command"))?;

    let prompt = format!("{}{}", prompt, taste_preamble());
    let mut parts: Vec<String> = shlex::split(command)
        .ok_or_else(|| anyhow::anyhow!("invalid connector command quoting"))?
        .into_iter()
        .map(|s| s.replace("{prompt}", &prompt))
        .collect();
    if parts.is_empty() {
        bail!("empty connector command");
    }

    let mut cmd = tokio::process::Command::new(&parts[0]);
    cmd.args(&parts[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &cfg.cwd {
        cmd.current_dir(cwd);
    }
    if let Some(ref key) = cfg.secret_env_key {
        if let Some(value) = crate::providers::resolve_env_key(key) {
            cmd.env(key, value);
        }
    }

    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut out = String::new();
    let mut err = String::new();
    let (read_out, read_err) =
        tokio::join!(async { stdout.read_to_string(&mut out).await }, async {
            stderr.read_to_string(&mut err).await
        });
    read_out?;
    read_err?;
    let status = child.wait().await?;

    if !out.is_empty() {
        println!("{out}");
    }
    if !err.is_empty() {
        eprintln!("{err}");
    }
    if !status.success() {
        bail!(
            "connector exited with status {}",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

async fn run_http_connector(cfg: &ConnectorConfig, url: &str, prompt: &str) -> Result<()> {
    use crate::net::{http_post_json, validate_url};
    let url = validate_url(url, false).await?;
    let mut headers = std::collections::HashMap::new();
    if let Some(ref key) = cfg.secret_env_key {
        if let Some(secret) = crate::providers::resolve_env_key(key) {
            headers.insert("Authorization".into(), format!("Bearer {secret}"));
        }
    }
    let body = serde_json::json!({ "prompt": format!("{}{}", prompt, taste_preamble()) });
    let (status, text) =
        http_post_json(&url, headers, body, std::time::Duration::from_secs(120)).await?;
    if status != 200 {
        bail!("connector HTTP {status}: {text}");
    }
    println!("{text}");
    Ok(())
}
