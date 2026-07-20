//! Cross-harness connector management for `omgb`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::args::HarnessType;
use crate::taste::taste_preamble;

const IS_WINDOWS: bool = cfg!(windows);

fn registry_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("connectors.json"))
}

fn apply_minimal_env(cmd: &mut tokio::process::Command) {
    cmd.env_clear();
    for key in [
        "HOME",
        "USERPROFILE",
        "SystemRoot",
        "SystemDrive",
        "TEMP",
        "TMP",
        "TMPDIR",
        "TERM",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "APPDATA",
        "LOCALAPPDATA",
        "USER",
        "USERNAME",
        "LOGNAME",
    ] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
}

fn base_dirs() -> Vec<PathBuf> {
    if IS_WINDOWS {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| String::from("C:\\Windows"));
        let root = PathBuf::from(root);
        [
            root.join("System32"),
            root.clone(),
            root.join("System32").join("Wbem"),
        ]
        .to_vec()
    } else {
        ["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
            .map(PathBuf::from)
            .to_vec()
    }
}

fn executable_extensions() -> Vec<String> {
    if IS_WINDOWS {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| {
                String::from(".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC;.PY")
            })
            .split(';')
            .map(|s| s.to_lowercase())
            .collect()
    } else {
        Vec::new()
    }
}

fn resolve_executable(name: &str, cwd: Option<&std::path::Path>) -> PathBuf {
    let candidate = PathBuf::from(name);
    if candidate.is_absolute() {
        return candidate;
    }

    let exts = if IS_WINDOWS {
        executable_extensions()
    } else {
        Vec::new()
    };
    let try_dir = |dir: &std::path::Path| -> Option<PathBuf> {
        let joined = dir.join(&candidate);
        if joined.is_file() {
            return Some(joined);
        }
        for ext in &exts {
            let with_ext = dir.join(format!("{name}{ext}"));
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
        None
    };

    let is_relative_path = candidate
        .components()
        .any(|c| matches!(c, std::path::Component::Normal(_)))
        && candidate.components().count() > 1;

    // Single-component names and relative paths are both resolved against cwd first,
    // then PATH, so connectors whose binaries live in the connector cwd work.
    if let Some(dir) = cwd {
        if let Some(p) = try_dir(dir) {
            return p;
        }
        if is_relative_path {
            return candidate;
        }
    }

    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(p) = try_dir(&dir) {
                return p;
            }
        }
    }
    candidate
}

fn minimal_path(binary_dir: Option<&std::path::Path>) -> String {
    let mut dirs = base_dirs();
    if let Some(dir) = binary_dir
        && !dirs.iter().any(|d| d.as_path() == dir)
    {
        dirs.insert(0, dir.to_path_buf());
    }
    std::env::join_paths(dirs)
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_default()
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
    serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
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
    crate::providers::restrict_env_file_permissions(&path)?;
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
    if let Some(ref key) = child_key
        && !crate::providers::is_valid_env_key(key)
    {
        bail!(
            "secret-env-key must end with _API_KEY and contain only uppercase A-Z, 0-9, and underscores"
        );
    }

    let storage = crate::providers::env_var_name(&name);
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
    save_registry(&registry)?;

    // API keys are only accepted via OMGB_API_KEY; persist the secret only after
    // the connector registry has been saved successfully.
    if let Some(key) = std::env::var("OMGB_API_KEY").ok().filter(|s| !s.is_empty()) {
        crate::providers::write_api_key(&storage, Some(std::slice::from_ref(&storage)), &key)?;
    }

    Ok(())
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

    let parent_cwd = std::env::current_dir()?;
    let resolve_dir = cfg
        .cwd
        .as_ref()
        .map(|c| parent_cwd.join(c))
        .or_else(|| Some(parent_cwd.clone()));
    let resolved = resolve_executable(&parts[0], resolve_dir.as_deref());
    let binary_dir = resolved.parent().map(|p| p.to_path_buf());

    let mut cmd = tokio::process::Command::new(&resolved);
    cmd.args(&parts[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &cfg.cwd {
        validate_cwd(cwd)?;
        cmd.current_dir(cwd);
    }
    apply_minimal_env(&mut cmd);
    cmd.env("PATH", minimal_path(binary_dir.as_deref()));

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
            false,
            false,
        );
        assert!(r.is_err(), "non-ASCII connector name should be rejected");
    }
}
