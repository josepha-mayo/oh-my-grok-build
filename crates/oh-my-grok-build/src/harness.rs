//! Cross-harness connector management for `omgb`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::args::HarnessType;
use crate::taste::taste_preamble;

fn registry_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("connectors.json"))
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
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_local: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_private: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn validate_cwd(cwd: &std::path::Path) -> Result<()> {
    if !cwd.is_absolute() {
        bail!("connector cwd must be an absolute path");
    }
    for comp in cwd.components() {
        if !matches!(
            comp,
            std::path::Component::Normal(_)
                | std::path::Component::Prefix(_)
                | std::path::Component::RootDir
        ) {
            bail!("connector cwd contains disallowed component: {comp:?}");
        }
    }
    if !cwd.exists() || !cwd.is_dir() {
        bail!(
            "connector cwd does not exist or is not a directory: {}",
            cwd.display()
        );
    }
    Ok(())
}

fn load_registry() -> Result<ConnectorRegistry> {
    let path = registry_path()?;
    if !path.exists() {
        return Ok(ConnectorRegistry::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{path}: {e}"))?)
}

fn save_registry(registry: &ConnectorRegistry) -> Result<()> {
    let path = registry_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("registry path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(registry)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn default_secret_env_key(r#type: &str) -> Option<&'static str> {
    match r#type {
        "codex" | "opencode" => Some("OPENAI_API_KEY"),
        "claude" => Some("ANTHROPIC_API_KEY"),
        "hermes" => Some("HERMES_API_KEY"),
        "pi" => Some("PI_API_KEY"),
        "omp" => Some("OMP_API_KEY"),
        _ => None,
    }
}

fn default_command(r#type: &str) -> Option<String> {
    match r#type {
        "codex" => Some("codex exec --json {prompt}".into()),
        "opencode" => Some("opencode run {prompt}".into()),
        "claude" => Some("claude {prompt}".into()),
        "hermes" => Some("hermes {prompt}".into()),
        "pi" => Some("pi {prompt}".into()),
        "omp" => Some("omp {prompt}".into()),
        _ => None,
    }
}

pub fn add_connector(
    name: String,
    r#type: HarnessType,
    mut command: Option<String>,
    url: Option<String>,
    cwd: Option<PathBuf>,
    api_key: Option<String>,
    secret_env_key: Option<String>,
    allow_local: bool,
    allow_private: bool,
) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("invalid connector name: must be ASCII alphanumeric, '_' or '-'");
    }
    let type_str = r#type.as_str().to_string();
    if command.is_some() && url.is_some() {
        bail!("connector cannot have both --command and --url");
    }
    if command.is_none() && url.is_none() {
        command = default_command(&type_str);
    }
    if command.is_none() && url.is_none() {
        bail!("connector requires --command or --url");
    }
    let mut registry = load_registry()?;

    let child_key = secret_env_key
        .clone()
        .or_else(|| default_secret_env_key(&type_str).map(|s| s.to_string()));
    if let Some(ref key) = child_key {
        if !crate::providers::is_valid_env_key(key) {
            bail!(
                "secret-env-key must end with _API_KEY and contain only uppercase A-Z, 0-9, and underscores"
            );
        }
    }

    if let Some(key) = api_key {
        if !key.is_empty() {
            let storage = crate::providers::env_var_name(&name);
            crate::providers::write_api_key(&name, Some(std::slice::from_ref(&storage)), &key)?;
        }
    }

    registry.connectors.insert(
        name.clone(),
        ConnectorConfig {
            name,
            r#type: type_str,
            command,
            url,
            cwd,
            secret_env_key: child_key,
            allow_local,
            allow_private,
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
    if cfg.is_some() {
        let storage = crate::providers::env_var_name(name);
        crate::providers::remove_api_key(name, true, Some(std::slice::from_ref(&storage)))?;
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
    let placeholder = "{prompt}";
    let mut found = false;
    let parts: Vec<String> = shlex::split(command)
        .ok_or_else(|| anyhow::anyhow!("invalid connector command quoting"))?
        .into_iter()
        .map(|s| {
            if s == placeholder {
                found = true;
                Ok(prompt.clone())
            } else if s.contains(placeholder) {
                bail!("{placeholder} must be a standalone argument in the connector command")
            } else {
                Ok(s)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    if parts.is_empty() {
        bail!("empty connector command");
    }
    if !found {
        bail!("connector command must contain {placeholder}");
    }

    let mut cmd = tokio::process::Command::new(&parts[0]);
    cmd.args(&parts[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &cfg.cwd {
        validate_cwd(cwd)?;
        cmd.current_dir(cwd);
    }
    let storage = crate::providers::env_var_name(name);
    if let Some(ref child_key) = cfg.secret_env_key {
        if !crate::providers::is_valid_env_key(child_key) {
            bail!("connector secret_env_key is invalid");
        }
        if let Some(value) = crate::providers::resolve_env_key(&storage)? {
            cmd.env(child_key, value);
        }
    }

    let mut child = cmd.spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("connector stdout was not piped"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("connector stderr was not piped"))?;
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
    let url = validate_url(url, cfg.allow_local, cfg.allow_private).await?;
    let mut headers = std::collections::HashMap::new();
    let storage = crate::providers::env_var_name(&cfg.name);
    if let Some(secret) = crate::providers::resolve_env_key(&storage)? {
        headers.insert("Authorization".into(), format!("Bearer {secret}"));
    }
    let body = serde_json::json!({ "prompt": format!("{}{}", prompt, taste_preamble()) });
    let (status, text) =
        http_post_json(&url, &headers, body, std::time::Duration::from_secs(120)).await?;
    if status != 200 {
        bail!("connector HTTP {status}: {text}");
    }
    println!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_connector_rejects_non_ascii_name() {
        let r = add_connector(
            "héllo".into(),
            HarnessType::Codex,
            Some("codex exec --json {prompt}".into()),
            None,
            None,
            None,
            None,
            false,
            false,
        );
        assert!(r.is_err(), "non-ASCII connector name should be rejected");
    }
}
