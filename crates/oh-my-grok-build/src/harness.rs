//! Cross-harness connector management for `omgb`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::args::HarnessType;
use crate::taste::taste_preamble;

const IS_WINDOWS: bool = cfg!(windows);
const DEFAULT_CONNECTOR_TIMEOUT: Duration = Duration::from_secs(60);

/// Allowed connector executable stems. The supported cross-harness CLIs are
/// exactly Codex, Claude, OpenCode, Hermes, Pi, and OMP. A connector command
/// must resolve to one of these basenames (after following symlinks) to prevent
/// shell-interpreter and command-wrapper injection.
const ALLOWED_CONNECTOR_STEMS: &[&str] = &["codex", "claude", "opencode", "hermes", "pi", "omp"];

/// Basenames that are explicitly rejected and called out in error messages.
const DENIED_CONNECTOR_BINARIES: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "csh",
    "ksh",
    "tcsh",
    "dash",
    "ash",
    "cmd",
    "command",
    "powershell",
    "pwsh",
    "python",
    "python3",
    "python2",
    "ruby",
    "perl",
    "php",
    "node",
    "nodejs",
    "lua",
    "osascript",
    "wscript",
    "cscript",
    "mshta",
    "npx",
    "npm",
    "yarn",
    "pnpm",
    "bun",
    "deno",
    "rm",
    "rmdir",
    "del",
    "erase",
    "rd",
    "dd",
    "mv",
    "move",
    "ren",
    "rename",
    "cp",
    "copy",
    "xcopy",
    "robocopy",
    "format",
    "mkfs",
    "fdisk",
    "fsutil",
    "chmod",
    "chown",
    "sudo",
    "su",
    "doas",
    "pkexec",
    "runas",
    "schtasks",
    "sc",
    "net",
    "ssh",
    "scp",
    "sftp",
    "ftp",
    "telnet",
    "nc",
    "netcat",
    "curl",
    "wget",
    "env",
    "nice",
    "ionice",
    "chrt",
    "taskset",
    "stdbuf",
    "timeout",
    "setsid",
    "script",
    "screen",
    "tmux",
    "xargs",
    "parallel",
    "find",
    "git",
    "make",
    "cmake",
    "ninja",
    "gcc",
    "cc",
    "clang",
    "rustc",
    "go",
    "javac",
    "java",
    "dotnet",
    "mono",
    "wine",
    "dosbox",
    "qemu",
    "tftp",
    "socat",
    "ncat",
    "rlsh",
    "rlogin",
    "rsh",
    "rexec",
    "ed",
    "ex",
    "sed",
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "vi",
    "vim",
    "emacs",
    "nano",
];

/// Basename prefixes used for quick rejection of interpreter families.
const DENIED_CONNECTOR_PREFIXES: &[&str] = &["python", "node", "npm", "yarn", "pnpm", "perl"];

fn first_token_stem(token: &str) -> String {
    std::path::Path::new(token)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(token)
        .to_ascii_lowercase()
}

fn allowed_connector_stem(stem: &str) -> bool {
    ALLOWED_CONNECTOR_STEMS
        .iter()
        .any(|s| s.eq_ignore_ascii_case(stem))
}

fn is_denied_connector_stem(stem: &str) -> bool {
    DENIED_CONNECTOR_BINARIES
        .iter()
        .any(|d| d.eq_ignore_ascii_case(stem))
        || DENIED_CONNECTOR_PREFIXES
            .iter()
            .any(|p| stem.starts_with(p))
}

/// Validate the connector command string. This is used both when the connector
/// is added and when it runs, and only permits the known cross-harness CLIs.
fn validate_connector_command(command: &str) -> Result<()> {
    let parts: Vec<String> = shlex::split(command)
        .ok_or_else(|| anyhow::anyhow!("invalid connector command quoting"))?
        .into_iter()
        .collect();
    if parts.is_empty() {
        bail!("empty connector command");
    }
    if !parts.iter().any(|p| p == "{prompt}") {
        bail!("connector command must contain {{prompt}}");
    }

    let first = &parts[0];
    let base = first_token_stem(first);
    if !allowed_connector_stem(&base) {
        if is_denied_connector_stem(&base) {
            bail!(
                "connector executable '{base}' is not allowed; use a harness CLI such as codex, claude, opencode, hermes, pi, or omp"
            );
        }
        bail!(
            "connector executable '{base}' is not an allowed harness CLI; allowed: codex, claude, opencode, hermes, pi, omp"
        );
    }
    Ok(())
}

/// Validate the resolved executable on the filesystem. This catches symlinks to
/// disallowed binaries and any executable (binary or script) that lives inside
/// the connector's working directory (including subdirectories and symlinks),
/// which would let an attacker drop a malicious file named after an allowed
/// harness. Install harness CLIs in PATH or reference them with an absolute
/// path outside the connector working directory.
fn validate_connector_executable(
    resolved: &std::path::Path,
    first_token: &str,
    resolve_dir: Option<&std::path::Path>,
) -> Result<()> {
    let canonical = dunce::canonicalize(resolved).with_context(|| {
        format!(
            "cannot canonicalize connector executable {}",
            resolved.display()
        )
    })?;

    let stem = canonical
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !allowed_connector_stem(&stem) {
        if is_denied_connector_stem(&stem) {
            bail!(
                "connector executable resolves to a disallowed binary '{stem}'; use a harness CLI such as codex, claude, opencode, hermes, pi, or omp"
            );
        }
        bail!("connector executable resolves to '{stem}', which is not an allowed harness CLI");
    }

    if let Some(dir) = resolve_dir {
        let canonical_dir = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let resolved_parent = resolved
            .parent()
            .ok_or_else(|| anyhow::anyhow!("connector executable has no parent directory"))?;
        let canonical_parent =
            dunce::canonicalize(resolved_parent).unwrap_or_else(|_| resolved_parent.to_path_buf());
        let target_parent = canonical
            .parent()
            .ok_or_else(|| anyhow::anyhow!("connector executable has no parent directory"))?;
        let canonical_target_parent =
            dunce::canonicalize(target_parent).unwrap_or_else(|_| target_parent.to_path_buf());
        if canonical_parent.starts_with(&canonical_dir)
            || canonical_target_parent.starts_with(&canonical_dir)
        {
            bail!(
                "connector executable {} ({}) is under the connector working directory; install the harness in PATH or use an absolute path outside this directory",
                resolved.display(),
                first_token
            );
        }
    }

    Ok(())
}

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
        let mut dirs = vec![
            root.join("System32"),
            root.clone(),
            root.join("System32").join("Wbem"),
        ];
        // Node-based connectors (e.g. codex/opencode) need node and global npm modules.
        if let Ok(pf) = std::env::var("ProgramFiles") {
            dirs.push(PathBuf::from(pf).join("nodejs"));
        }
        if let Ok(pf_x86) = std::env::var("ProgramFiles(x86)") {
            dirs.push(PathBuf::from(pf_x86).join("nodejs"));
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
        dirs
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

fn is_executable_file(path: &std::path::Path, exts: &[String]) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.iter().any(|ext| ext.eq_ignore_ascii_case(e)))
    }
    #[cfg(not(any(unix, windows)))]
    false
}

fn resolve_executable(name: &str, cwd: Option<&std::path::Path>) -> Result<PathBuf> {
    let candidate = PathBuf::from(name);
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!(
            "connector command must not contain '..' components: {}",
            candidate.display()
        );
    }

    let exts = if IS_WINDOWS {
        executable_extensions()
    } else {
        Vec::new()
    };

    if candidate.is_absolute() {
        if is_executable_file(&candidate, &exts) {
            return Ok(candidate);
        }
        // On Windows the user may have omitted the extension (e.g. "codex" when
        // "codex.exe" exists). Try the PATHEXT extensions in the same directory.
        if IS_WINDOWS
            && let (Some(parent), Some(stem)) = (candidate.parent(), candidate.file_stem())
        {
            let stem = stem.to_string_lossy();
            for ext in &exts {
                let with_ext = parent.join(format!("{stem}{ext}"));
                if is_executable_file(&with_ext, &exts) {
                    return Ok(with_ext);
                }
            }
        }
        bail!(
            "connector command not found or is not executable: {}",
            candidate.display()
        );
    }

    let try_dir = |dir: &std::path::Path| -> Option<PathBuf> {
        let joined = dir.join(&candidate);
        if is_executable_file(&joined, &exts) {
            return Some(joined);
        }
        for ext in &exts {
            let with_ext = dir.join(format!("{name}{ext}"));
            if is_executable_file(&with_ext, &exts) {
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
    // then PATH, then the curated minimal PATH directories, so connectors whose
    // binaries live in the connector cwd or in standard install locations work.
    if let Some(dir) = cwd {
        if let Some(p) = try_dir(dir) {
            return Ok(p);
        }
        if is_relative_path {
            bail!(
                "connector command not found in connector cwd: {}",
                candidate.display()
            );
        }
    }

    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(p) = try_dir(&dir) {
                return Ok(p);
            }
        }
    }

    for dir in base_dirs() {
        if let Some(p) = try_dir(&dir) {
            return Ok(p);
        }
    }

    bail!("connector command not found: {}", candidate.display())
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
    crate::providers::write_file_atomic(&path, serde_json::to_string_pretty(registry)?, true)
}

fn default_secret_env_key(r#type: &str) -> Option<&'static str> {
    match r#type {
        // OpenAI Codex CLI uses OPENAI_API_KEY by default.
        "codex" => Some("OPENAI_API_KEY"),
        // Claude Code uses ANTHROPIC_API_KEY.
        "claude" => Some("ANTHROPIC_API_KEY"),
        // Hermes Agent's documented default/recommended provider is OpenRouter.
        "hermes" => Some("OPENROUTER_API_KEY"),
        // OpenCode picks Anthropic before OpenAI when both are present, so
        // ANTHROPIC_API_KEY is the most useful default to pass through.
        "opencode" => Some("ANTHROPIC_API_KEY"),
        // Pi and OMP are multi-provider wrappers; users should pass
        // --secret-env-key or set the provider-specific env var.
        _ => None,
    }
}

fn default_command(r#type: &str) -> Option<String> {
    match r#type {
        "codex" => Some("codex exec --json {prompt}".into()),
        "opencode" => Some("opencode run {prompt}".into()),
        // Claude Code only runs a prompt non-interactively with --print.
        "claude" => Some("claude --print {prompt}".into()),
        // Hermes one-shot mode (-z) outputs only the final response.
        "hermes" => Some("hermes -z {prompt}".into()),
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
    if let Some(ref cmd) = command {
        validate_connector_command(cmd)?;
    }
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
    let secret = std::env::var("OMGB_API_KEY")
        .ok()
        .filter(|value| !value.is_empty());
    let _lock = crate::providers::provider_mutation_lock()?;
    let mut registry = load_registry()?;
    let previous = registry.clone();
    registry.connectors.insert(
        name.clone(),
        ConnectorConfig {
            name: name.clone(),
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
    if let Some(key) = secret
        && let Err(error) = crate::providers::write_api_key_unlocked(
            &name,
            Some(std::slice::from_ref(&storage)),
            &key,
        )
    {
        save_registry(&previous)
            .context("roll back connector registry after secret write failed")?;
        return Err(error);
    }

    Ok(())
}

pub fn list_connectors() -> Result<Vec<ConnectorConfig>> {
    Ok(load_registry()?.connectors.values().cloned().collect())
}

pub fn remove_connector(name: &str) -> Result<()> {
    let _lock = crate::providers::provider_mutation_lock()?;
    let mut registry = load_registry()?;
    let previous = registry.clone();
    let cfg = registry.connectors.remove(name);
    save_registry(&registry)?;
    if cfg.is_some() {
        let storage = crate::providers::env_var_name(name);
        if let Err(error) = crate::providers::remove_api_key_unlocked(
            name,
            true,
            Some(std::slice::from_ref(&storage)),
        ) {
            save_registry(&previous)
                .context("roll back connector registry after secret removal failed")?;
            return Err(error);
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
    validate_connector_command(command)?;

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
    let resolved = resolve_executable(&parts[0], resolve_dir.as_deref())?;
    validate_connector_executable(&resolved, &parts[0], resolve_dir.as_deref())?;
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

    let (mut child, group) = crate::spawn_with_process_group(cmd)?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("connector stdout was not piped"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("connector stderr was not piped"))?;
    let out_handle = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s).await;
        s
    });
    let err_handle = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });

    let status = match tokio::time::timeout(DEFAULT_CONNECTOR_TIMEOUT, child.wait()).await {
        Ok(s) => s?,
        Err(_) => {
            crate::kill_child_and_reap(&mut child, group.as_ref()).await;
            out_handle.abort();
            err_handle.abort();
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(out_handle, err_handle)
            })
            .await;
            bail!(
                "connector '{name}' timed out after {}s",
                DEFAULT_CONNECTOR_TIMEOUT.as_secs()
            );
        }
    };

    crate::kill_process_group(group.as_ref());
    let (out, err) = tokio::join!(out_handle, err_handle);
    let out = out.unwrap_or_default();
    let err = err.unwrap_or_default();
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
    let (status, text) = http_post_json(&url, &headers, body, DEFAULT_CONNECTOR_TIMEOUT).await?;
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
    fn test_default_secret_env_key_matches_cli_docs() {
        assert_eq!(default_secret_env_key("codex"), Some("OPENAI_API_KEY"));
        assert_eq!(default_secret_env_key("claude"), Some("ANTHROPIC_API_KEY"));
        assert_eq!(default_secret_env_key("hermes"), Some("OPENROUTER_API_KEY"));
        assert_eq!(
            default_secret_env_key("opencode"),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(default_secret_env_key("pi"), None);
        assert_eq!(default_secret_env_key("omp"), None);
    }

    #[test]
    fn test_add_connector_rejects_non_ascii_name() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("omgb-test-{}-harness", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        crate::providers::set_omg_home_for_tests(Some(tmp.clone()));
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
        crate::providers::set_omg_home_for_tests(None);
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(r.is_err(), "non-ASCII connector name should be rejected");
    }

    #[test]
    fn concurrent_connector_mutations_preserve_unrelated_entries() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let tmp = temp_dir_for_test();
        crate::providers::set_omg_home_for_tests(Some(tmp.clone()));
        let add = |name: &str| {
            add_connector(
                name.into(),
                HarnessType::Codex,
                Some("codex exec --json {prompt}".into()),
                None,
                None,
                None,
                false,
                false,
            )
        };

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let writers: Vec<_> = ["alpha", "beta"]
            .into_iter()
            .map(|name| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    add_connector(
                        name.into(),
                        HarnessType::Codex,
                        Some("codex exec --json {prompt}".into()),
                        None,
                        None,
                        None,
                        false,
                        false,
                    )
                })
            })
            .collect();
        barrier.wait();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }
        let registry = load_registry().unwrap();
        assert!(registry.connectors.contains_key("alpha"));
        assert!(registry.connectors.contains_key("beta"));

        add("remove-me").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let add_barrier = barrier.clone();
        let add_thread = std::thread::spawn(move || {
            add_barrier.wait();
            add_connector(
                "gamma".into(),
                HarnessType::Codex,
                Some("codex exec --json {prompt}".into()),
                None,
                None,
                None,
                false,
                false,
            )
        });
        let remove_barrier = barrier.clone();
        let remove_thread = std::thread::spawn(move || {
            remove_barrier.wait();
            remove_connector("remove-me")
        });
        barrier.wait();
        add_thread.join().unwrap().unwrap();
        remove_thread.join().unwrap().unwrap();

        let registry = load_registry().unwrap();
        assert!(registry.connectors.contains_key("alpha"));
        assert!(registry.connectors.contains_key("beta"));
        assert!(registry.connectors.contains_key("gamma"));
        assert!(!registry.connectors.contains_key("remove-me"));

        crate::providers::set_omg_home_for_tests(None);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn test_validate_connector_command_accepts_harnesses() {
        for cmd in [
            "codex exec --json {prompt}",
            "claude --print {prompt}",
            "opencode run {prompt}",
            "hermes -z {prompt}",
            "pi {prompt}",
            "omp {prompt}",
            "/usr/local/bin/codex exec --json {prompt}",
        ] {
            assert!(
                validate_connector_command(cmd).is_ok(),
                "{cmd} should be accepted"
            );
        }
    }

    #[test]
    fn test_validate_connector_command_rejects_wrappers_and_interpreters() {
        for cmd in [
            "env sh -c {prompt}",
            "env python -c {prompt}",
            "nice codex {prompt}",
            "chrt 1 codex {prompt}",
            "stdbuf -o0 sh -c {prompt}",
            "timeout 10 python -c {prompt}",
            "setsid sh -c {prompt}",
            "xargs sh -c {prompt}",
            "find . -exec sh {} {prompt} ;",
            "sh -c {prompt}",
            "bash {prompt}",
            "python3 -c {prompt}",
        ] {
            assert!(
                validate_connector_command(cmd).is_err(),
                "{cmd} should be rejected"
            );
        }
    }

    #[test]
    fn test_validate_connector_command_rejects_arbitrary_and_missing_prompt() {
        assert!(validate_connector_command("myagent {prompt}").is_err());
        assert!(validate_connector_command("codex exec --json").is_err());
        assert!(validate_connector_command("").is_err());
    }

    fn temp_dir_for_test() -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("omgb-harness-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_connector_executable_rejects_symlink_to_denied() {
        use std::os::unix::fs::symlink;
        let tmp = temp_dir_for_test();
        let denied = tmp.join("sh");
        std::fs::write(&denied, "#!/bin/sh\n").unwrap();
        let codex = tmp.join("codex");
        symlink(&denied, &codex).unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&tmp));
        assert!(r.is_err(), "symlink to denied binary should be rejected");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn outside_dir_for_test(tmp: &std::path::Path) -> PathBuf {
        tmp.parent()
            .unwrap_or(&std::env::temp_dir())
            .join(format!("omgb-harness-outside-{}", uuid::Uuid::new_v4()))
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_connector_executable_rejects_script_in_cwd() {
        let tmp = temp_dir_for_test();
        let codex = tmp.join("codex");
        std::fs::write(&codex, "#!/usr/bin/env node\n").unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&tmp));
        assert!(r.is_err(), "script in connector cwd should be rejected");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_connector_executable_rejects_script_in_subdir() {
        let tmp = temp_dir_for_test();
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex");
        std::fs::write(&codex, "#!/usr/bin/env node\n").unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&tmp));
        assert!(
            r.is_err(),
            "script in a subdirectory of connector cwd should be rejected"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_connector_executable_rejects_symlink_in_cwd() {
        use std::os::unix::fs::symlink;
        let tmp = temp_dir_for_test();
        let outside = outside_dir_for_test(&tmp);
        std::fs::create_dir_all(&outside).unwrap();
        let real = outside.join("codex");
        std::fs::write(&real, "#!/usr/bin/env node\n").unwrap();
        let link = tmp.join("codex");
        symlink(&real, &link).unwrap();

        let r = validate_connector_executable(&link, "codex", Some(&tmp));
        assert!(
            r.is_err(),
            "symlink to a script in connector cwd should be rejected"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_connector_executable_accepts_script_outside_cwd() {
        let tmp = temp_dir_for_test();
        let resolve_dir = tmp.join("cwd");
        std::fs::create_dir_all(&resolve_dir).unwrap();
        let outside = outside_dir_for_test(&tmp);
        std::fs::create_dir_all(&outside).unwrap();
        let codex = outside.join("codex");
        std::fs::write(&codex, "#!/usr/bin/env node\n").unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&resolve_dir));
        assert!(
            r.is_ok(),
            "script outside connector cwd should be accepted: {r:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_connector_executable_accepts_symlink_outside_cwd() {
        use std::os::unix::fs::symlink;
        let tmp = temp_dir_for_test();
        let resolve_dir = tmp.join("cwd");
        std::fs::create_dir_all(&resolve_dir).unwrap();
        let outside = outside_dir_for_test(&tmp);
        std::fs::create_dir_all(&outside).unwrap();
        let real = outside.join("codex");
        std::fs::write(&real, "#!/usr/bin/env node\n").unwrap();
        let link = outside.join("codex-link");
        symlink(&real, &link).unwrap();

        let r = validate_connector_executable(&link, "codex", Some(&resolve_dir));
        assert!(
            r.is_ok(),
            "symlink to a script outside connector cwd should be accepted: {r:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_connector_executable_rejects_binary_in_cwd() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = temp_dir_for_test();
        let codex = tmp.join("codex");
        std::fs::write(&codex, &[0x7f, b'E', b'L', b'F']).unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&tmp));
        assert!(
            r.is_err(),
            "binary in connector cwd should be rejected: {r:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_connector_executable_accepts_binary_outside_cwd() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = temp_dir_for_test();
        let resolve_dir = tmp.join("cwd");
        std::fs::create_dir_all(&resolve_dir).unwrap();
        let outside = outside_dir_for_test(&tmp);
        std::fs::create_dir_all(&outside).unwrap();
        let codex = outside.join("codex");
        std::fs::write(&codex, &[0x7f, b'E', b'L', b'F']).unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&resolve_dir));
        assert!(
            r.is_ok(),
            "binary outside connector cwd should be accepted: {r:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(windows)]
    #[test]
    fn test_validate_connector_executable_rejects_cmd_in_cwd() {
        let tmp = temp_dir_for_test();
        let codex = tmp.join("codex.cmd");
        std::fs::write(&codex, "@echo off\n").unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&tmp));
        assert!(
            r.is_err(),
            ".cmd script in connector cwd should be rejected"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(windows)]
    #[test]
    fn test_validate_connector_executable_rejects_cmd_in_subdir() {
        let tmp = temp_dir_for_test();
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex.cmd");
        std::fs::write(&codex, "@echo off\n").unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&tmp));
        assert!(
            r.is_err(),
            ".cmd script in a subdirectory of connector cwd should be rejected"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(windows)]
    #[test]
    fn test_validate_connector_executable_accepts_cmd_outside_cwd() {
        let tmp = temp_dir_for_test();
        let resolve_dir = tmp.join("cwd");
        std::fs::create_dir_all(&resolve_dir).unwrap();
        let outside = outside_dir_for_test(&tmp);
        std::fs::create_dir_all(&outside).unwrap();
        let codex = outside.join("codex.cmd");
        std::fs::write(&codex, "@echo off\n").unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&resolve_dir));
        assert!(
            r.is_ok(),
            ".cmd script outside connector cwd should be accepted: {r:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(windows)]
    #[test]
    fn test_validate_connector_executable_rejects_binary_in_cwd() {
        let tmp = temp_dir_for_test();
        let codex = tmp.join("codex.exe");
        std::fs::write(&codex, b"MZ\x90\x00").unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&tmp));
        assert!(
            r.is_err(),
            ".exe binary in connector cwd should be rejected: {r:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(windows)]
    #[test]
    fn test_validate_connector_executable_accepts_binary_outside_cwd() {
        let tmp = temp_dir_for_test();
        let resolve_dir = tmp.join("cwd");
        std::fs::create_dir_all(&resolve_dir).unwrap();
        let outside = outside_dir_for_test(&tmp);
        std::fs::create_dir_all(&outside).unwrap();
        let codex = outside.join("codex.exe");
        std::fs::write(&codex, b"MZ\x90\x00").unwrap();

        let r = validate_connector_executable(&codex, "codex", Some(&resolve_dir));
        assert!(
            r.is_ok(),
            ".exe binary outside connector cwd should be accepted: {r:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn test_add_connector_allows_harness_and_rejects_wrapper() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let tmp = temp_dir_for_test();
        crate::providers::set_omg_home_for_tests(Some(tmp.clone()));

        let ok = add_connector(
            "codex-test".into(),
            HarnessType::Codex,
            Some("codex exec --json {prompt}".into()),
            None,
            None,
            None,
            false,
            false,
        );
        assert!(
            ok.is_ok(),
            "valid codex connector should be accepted: {ok:?}"
        );

        let bad = add_connector(
            "bad".into(),
            HarnessType::Codex,
            Some("env python -c {prompt}".into()),
            None,
            None,
            None,
            false,
            false,
        );
        assert!(bad.is_err(), "wrapper connector should be rejected");

        crate::providers::set_omg_home_for_tests(None);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
