//! BYOK and local-model provider management for `omgb`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::args::{AddProviderArgs, DiscoverArgs};
use crate::net::{http_get_text, http_post_json, is_url_host_private, validate_url};
use url::Url;

fn is_false(value: &bool) -> bool {
    !*value
}

pub mod catalog;

const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434/v1";
const DEFAULT_LMSTUDIO_URL: &str = "http://localhost:1234/v1";
const DEFAULT_VLLM_URL: &str = "http://localhost:8000/v1";
const DEFAULT_LLAMA_CPP_URL: &str = "http://localhost:8080/v1";
const DEFAULT_SGLANG_URL: &str = "http://localhost:30000/v1";
const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

const LOCAL_PROVIDER_IDS: &[&str] = &[
    "ollama",
    "lmstudio",
    "vllm",
    "llama-cpp",
    "tabby",
    "jan",
    "localai",
    "llamafile",
    "text-generation-webui",
    "koboldcpp",
    "mistral-rs",
    "sglang",
    "mlc-llm",
    "xinference",
    "aphrodite",
    "litellm",
    "text-generation-inference",
    "lorax",
];

pub(crate) fn is_local_provider_id(id: &str) -> bool {
    LOCAL_PROVIDER_IDS
        .iter()
        .any(|prefix| *prefix == id || id.starts_with(&format!("{prefix}-")))
        || id == "local"
        || id.starts_with("local-")
}

#[derive(Debug, Clone)]
pub struct ModelListEntry {
    pub id: String,
    pub context_window: Option<u64>,
}

pub fn omg_dir() -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(override_path) = OMGB_HOME_OVERRIDE.lock().unwrap().as_ref() {
        return Ok(override_path.clone());
    }
    if let Ok(v) = std::env::var("OMGB_HOME") {
        return Ok(PathBuf::from(v));
    }
    dirs::home_dir()
        .map(|h| h.join(".omgb"))
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory; set OMGB_HOME"))
}

#[cfg(test)]
pub(crate) static OMGB_HOME_OVERRIDE: std::sync::Mutex<Option<PathBuf>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_omg_home_for_tests(path: Option<PathBuf>) {
    *OMGB_HOME_OVERRIDE.lock().unwrap() = path;
}

fn omg_config_path() -> Result<PathBuf> {
    Ok(omg_dir()?.join("config.json"))
}

fn omg_env_path() -> Result<PathBuf> {
    Ok(omg_dir()?.join(".env"))
}

fn grok_home() -> PathBuf {
    #[cfg(test)]
    if let Some(override_path) = GROK_HOME_OVERRIDE.lock().unwrap().as_ref() {
        return override_path.clone();
    }
    xai_grok_shell::util::grok_home::grok_home()
}

#[cfg(test)]
static GROK_HOME_OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn set_grok_home_for_tests(path: Option<PathBuf>) {
    *GROK_HOME_OVERRIDE.lock().unwrap() = path;
}

fn grok_config_path() -> PathBuf {
    grok_home().join("config.toml")
}

pub(crate) struct ProviderMutationGuard {
    _files: Vec<std::fs::File>,
}

pub(crate) fn provider_mutation_lock() -> Result<ProviderMutationGuard> {
    let mut dirs = vec![omg_dir()?, grok_home()];
    for dir in &dirs {
        std::fs::create_dir_all(dir)?;
        restrict_omg_directory_permissions(dir)?;
    }
    dirs = dirs
        .into_iter()
        .map(|dir| dunce::canonicalize(&dir).unwrap_or(dir))
        .collect();
    dirs.sort();
    dirs.dedup();

    let mut files = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let path = dir.join("omgb-provider-mutation.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        restrict_omg_file_permissions(&path)?;
        FileExt::lock_exclusive(&file)?;
        files.push(file);
    }
    Ok(ProviderMutationGuard { _files: files })
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
    /// The endpoint intentionally accepts requests without credentials.
    /// This is inferred only for verified loopback providers so a Grok
    /// session token is never substituted for a missing local-provider key.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_auth: bool,
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

#[cfg(test)]
pub fn save_omg_config(config: &OmgConfig) -> Result<()> {
    let _lock = provider_mutation_lock()?;
    save_omg_config_unlocked(config)
}

fn save_omg_config_unlocked(config: &OmgConfig) -> Result<()> {
    let path = omg_config_path()?;
    write_file_atomic(&path, serde_json::to_string_pretty(config)?, true)
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
/// configured provider, connector, known catalog template, or built-in web
/// search integration are loaded, and only when a non-empty value is present
/// in `~/.omgb/.env`. This limits the secrets visible to child processes while
/// still letting upstream Grok Build resolve `env_key` references and letting
/// catalog-based MoE routing discover keys before a provider is persisted.
pub(crate) fn env_keys_to_load() -> HashSet<String> {
    let mut keys = HashSet::new();
    for k in [
        "TAVILY_API_KEY",
        "BRAVE_API_KEY",
        "SERPER_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_CX",
        "BING_API_KEY",
        "SEARXNG_URL",
    ] {
        keys.insert(k.to_string());
    }

    if let Ok(providers) = list_providers() {
        for p in &providers {
            for k in valid_env_keys(p) {
                keys.insert(k);
            }
        }
    }
    for t in catalog::TEMPLATES {
        for k in valid_env_keys(&t.to_provider_config()) {
            keys.insert(k);
        }
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

pub(crate) fn write_file_atomic(
    path: &std::path::Path,
    content: impl AsRef<[u8]>,
    restrict: bool,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    if restrict {
        restrict_omg_directory_permissions(parent)?;
        if path.exists() {
            // ReplaceFileW intentionally preserves an existing destination's
            // DACL. Tighten it before publishing secret bytes so there is no
            // broad-access window between replacement and a later ACL update.
            restrict_omg_file_permissions(path)?;
        }
    }
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().to_string().replace('-', "")
    ));
    let write = || -> Result<()> {
        use std::io::Write as _;

        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            use std::os::unix::fs::PermissionsExt;
            if restrict {
                options.mode(0o600);
            } else if let Ok(metadata) = std::fs::metadata(path) {
                options.mode(metadata.permissions().mode());
            }
        }
        let mut file = options.open(&tmp)?;
        if restrict {
            restrict_omg_file_permissions(&tmp)?;
        }
        file.write_all(content.as_ref())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        replace_file_atomic_windows(&tmp, path)?;
        #[cfg(not(windows))]
        std::fs::rename(&tmp, path)?;
        if restrict {
            // Verify the final path has the intended metadata even on
            // platforms whose atomic replacement inherits destination state.
            restrict_omg_file_permissions(path)?;
        }
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Atomically publishes `content` only if the bytes displaced at the publish
/// point equal `expected`. The platform swap keeps the previous file reachable
/// until after it is verified, closing the read-then-rename lost-update window.
pub(crate) fn write_file_atomic_if_unchanged(
    path: &std::path::Path,
    expected: &[u8],
    content: &[u8],
) -> Result<()> {
    use std::io::Write as _;

    #[cfg(unix)]
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    let token = uuid::Uuid::new_v4().to_string().replace('-', "");
    let staged = path.with_extension(format!("omgb-cas-{token}.new"));
    let displaced = path.with_extension(format!("omgb-cas-{token}.old"));
    let write = || -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(std::fs::metadata(path)?.permissions().mode());
        }
        let mut file = options.open(&staged)?;
        file.write_all(content)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);

        atomic_exchange_with_backup(&staged, path, &displaced)?;
        let previous = std::fs::read(&displaced)?;
        if previous != expected {
            let current = std::fs::read(path)?;
            if current == content {
                atomic_exchange_with_backup(&displaced, path, &staged)?;
                let _ = std::fs::remove_file(&staged);
            } else {
                bail!(
                    "file changed during atomic publish; the displaced version is preserved at {}",
                    displaced.display()
                );
            }
            bail!("file changed during atomic publish: {}", path.display());
        }
        std::fs::remove_file(&displaced)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    };
    if let Err(error) = write() {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    Ok(())
}

#[cfg(windows)]
fn atomic_exchange_with_backup(
    replacement: &std::path::Path,
    destination: &std::path::Path,
    backup: &std::path::Path,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW};
    use windows::core::PCWSTR;

    let wide = |path: &std::path::Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<u16>>()
    };
    let replacement = wide(replacement);
    let destination = wide(destination);
    let backup = wide(backup);
    unsafe {
        ReplaceFileW(
            PCWSTR(destination.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR(backup.as_ptr()),
            REPLACEFILE_WRITE_THROUGH,
            None,
            None,
        )
    }
    .context("atomic conditional Windows file replacement failed")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn atomic_exchange_with_backup(
    replacement: &std::path::Path,
    destination: &std::path::Path,
    backup: &std::path::Path,
) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    let replacement_path = replacement.to_path_buf();
    let replacement = CString::new(replacement_path.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            replacement.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .context("atomic conditional file exchange failed");
    }
    // The displaced destination now occupies the replacement path.
    std::fs::rename(replacement_path, backup)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn atomic_exchange_with_backup(
    replacement: &std::path::Path,
    destination: &std::path::Path,
    backup: &std::path::Path,
) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    unsafe extern "C" {
        fn renamex_np(
            from: *const libc::c_char,
            to: *const libc::c_char,
            flags: u32,
        ) -> libc::c_int;
    }
    const RENAME_SWAP: u32 = 0x0000_0002;
    let replacement_path = replacement.to_path_buf();
    let replacement = CString::new(replacement_path.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    let result = unsafe { renamex_np(replacement.as_ptr(), destination.as_ptr(), RENAME_SWAP) };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .context("atomic conditional file exchange failed");
    }
    std::fs::rename(replacement_path, backup)?;
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn atomic_exchange_with_backup(
    _replacement: &std::path::Path,
    _destination: &std::path::Path,
    _backup: &std::path::Path,
) -> Result<()> {
    bail!("atomic conditional file exchange is unsupported on this Unix platform")
}

#[cfg(windows)]
fn replace_file_atomic_windows(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACEFILE_WRITE_THROUGH,
        ReplaceFileW,
    };
    use windows::core::PCWSTR;

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if destination_exists(destination.as_slice()) {
        unsafe {
            ReplaceFileW(
                PCWSTR(destination.as_ptr()),
                PCWSTR(source.as_ptr()),
                PCWSTR::null(),
                REPLACEFILE_WRITE_THROUGH,
                None,
                None,
            )
        }
        .context("atomic Windows file replacement failed")?;
    } else {
        unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(destination.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .context("atomic Windows file publication failed")?;
    }
    Ok(())
}

#[cfg(windows)]
fn destination_exists(path: &[u16]) -> bool {
    use windows::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
    use windows::core::PCWSTR;
    unsafe { GetFileAttributesW(PCWSTR(path.as_ptr())) != INVALID_FILE_ATTRIBUTES }
}

fn write_env_entries(entries: &[(String, String)]) -> Result<()> {
    let path = omg_env_path()?;
    let mut content = String::new();
    for (k, v) in entries {
        content.push_str(&format!("{}={}\n", k, format_env_value(v)));
    }
    write_file_atomic(&path, content, true)
}

pub(crate) fn restrict_omg_file_permissions(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        windows_restrict_file_permissions(path)?;
    }
    Ok(())
}

pub(crate) fn restrict_omg_directory_permissions(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    {
        windows_restrict_file_permissions(path)?;
    }
    Ok(())
}

#[cfg(windows)]
fn windows_restrict_file_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{CloseHandle, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
        TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        ACE_FLAGS, ACL, DACL_SECURITY_INFORMATION, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TokenUser,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::PCWSTR;

    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect permissions target {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing to change permissions through symlink: {}",
            path.display()
        );
    }
    // OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE. A protected directory DACL
    // must pass the owner ACE to newly-created lock/temp files; otherwise they
    // receive an empty DACL and become inaccessible on their next open.
    let inheritance = if metadata.is_dir() {
        ACE_FLAGS(0x1 | 0x2)
    } else {
        ACE_FLAGS(0)
    };

    unsafe {
        let mut token_handle = windows::Win32::Foundation::HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle)
            .map_err(|e| anyhow::anyhow!("OpenProcessToken failed: {e}"))?;

        let mut return_length = 0u32;
        let _ = GetTokenInformation(token_handle, TokenUser, None, 0, &mut return_length);

        let mut token_user_buffer = vec![0u8; return_length as usize];
        GetTokenInformation(
            token_handle,
            TokenUser,
            Some(token_user_buffer.as_mut_ptr() as *mut _),
            return_length,
            &mut return_length,
        )
        .map_err(|e| {
            let _ = CloseHandle(token_handle);
            anyhow::anyhow!("GetTokenInformation failed: {e}")
        })?;

        // TOKEN_USER begins with a SID_AND_ATTRIBUTES whose first field is the PSID.
        // Read it without creating an under-aligned reference.
        let user_sid = crate::win_sid::sid_ptr(&token_user_buffer).inspect_err(|_| {
            let _ = CloseHandle(token_handle);
        })?;

        let explicit_access = EXPLICIT_ACCESS_W {
            // Use the concrete file access mask rather than GENERIC_ALL. A
            // protected DACL containing an unmapped generic bit can deny the
            // owner WRITE_DAC on the next attempt to tighten the same file.
            grfAccessPermissions: 0x001F01FF, // FILE_ALL_ACCESS
            grfAccessMode: SET_ACCESS,
            grfInheritance: inheritance,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation:
                    windows::Win32::Security::Authorization::NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: windows::core::PWSTR(user_sid.0 as *mut u16),
            },
        };

        let mut new_acl: *mut ACL = std::ptr::null_mut();
        let result = SetEntriesInAclW(Some(&[explicit_access]), None, &mut new_acl);
        if result.0 != 0 {
            let _ = CloseHandle(token_handle);
            bail!("SetEntriesInAclW failed: {}", result.0);
        }

        let wide_path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let result = SetNamedSecurityInfoW(
            PCWSTR::from_raw(wide_path.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_acl),
            None,
        );

        let _ = LocalFree(Some(HLOCAL(new_acl as *mut _)));
        let _ = CloseHandle(token_handle);

        if result.0 != 0 {
            bail!("SetNamedSecurityInfoW failed: {}", result.0);
        }
    }

    Ok(())
}

#[cfg(windows)]
pub(crate) fn windows_permissions_restriction_issue(
    path: &std::path::Path,
) -> Result<Option<String>> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
        GetSecurityDescriptorControl, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SE_DACL_PROTECTED,
    };
    use windows::core::PCWSTR;

    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect permissions target {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "permissions target is not a regular file: {}",
            path.display()
        );
    }
    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut owner = PSID::default();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let status = GetNamedSecurityInfoW(
            PCWSTR::from_raw(wide_path.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            &mut descriptor,
        );
        if status.0 != 0 {
            bail!("GetNamedSecurityInfoW failed: {}", status.0);
        }

        let inspection = (|| -> Result<Option<String>> {
            if descriptor.is_invalid() || owner.is_invalid() || dacl.is_null() {
                return Ok(Some("Windows file ACL is missing an owner or DACL".into()));
            }
            let mut control = 0u16;
            let mut revision = 0u32;
            GetSecurityDescriptorControl(descriptor, &mut control, &mut revision)
                .map_err(|error| anyhow::anyhow!("GetSecurityDescriptorControl failed: {error}"))?;
            if control & SE_DACL_PROTECTED.0 == 0 {
                return Ok(Some(
                    "Windows file ACL inherits access from its parent".into(),
                ));
            }

            let mut info = ACL_SIZE_INFORMATION::default();
            GetAclInformation(
                dacl,
                &mut info as *mut _ as *mut _,
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
            .map_err(|error| anyhow::anyhow!("GetAclInformation failed: {error}"))?;
            if info.AceCount != 1 {
                return Ok(Some(format!(
                    "Windows file ACL grants access through {} entries; expected only the owner",
                    info.AceCount
                )));
            }

            let mut raw_ace = std::ptr::null_mut();
            GetAce(dacl, 0, &mut raw_ace)
                .map_err(|error| anyhow::anyhow!("GetAce failed: {error}"))?;
            if raw_ace.is_null() {
                return Ok(Some("Windows file ACL has an empty access entry".into()));
            }
            let header = &*(raw_ace as *const windows::Win32::Security::ACE_HEADER);
            if header.AceType != 0
                || usize::from(header.AceSize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>()
            {
                return Ok(Some(
                    "Windows file ACL does not contain one owner allow entry".into(),
                ));
            }
            let allowed = &*(raw_ace as *const ACCESS_ALLOWED_ACE);
            if allowed.Mask != 0x001F01FF {
                return Ok(Some(
                    "Windows owner ACL does not grant the expected private file access".into(),
                ));
            }
            let ace_sid = PSID(std::ptr::addr_of!(allowed.SidStart) as *mut _);
            if EqualSid(owner, ace_sid).is_err() {
                return Ok(Some(
                    "Windows file ACL grants access to an identity other than its owner".into(),
                ));
            }
            Ok(None)
        })();

        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        inspection
    }
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
    if key.is_empty() {
        return false;
    }
    let allowed_names = ["GOOGLE_CX", "SEARXNG_URL"];
    if allowed_names.contains(&key) {
        return true;
    }
    key.ends_with("_API_KEY")
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

#[cfg(test)]
pub fn write_api_key(provider_id: &str, env_keys: Option<&[String]>, key: &str) -> Result<String> {
    let _lock = provider_mutation_lock()?;
    write_api_key_unlocked(provider_id, env_keys, key)
}

pub(crate) fn write_api_key_unlocked(
    provider_id: &str,
    env_keys: Option<&[String]>,
    key: &str,
) -> Result<String> {
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
    for provider in list_providers()? {
        if exclude_provider == Some(provider.id.as_str()) {
            continue;
        }
        if provider
            .env_key
            .as_ref()
            .is_some_and(|keys| keys.iter().any(|k| k == target))
        {
            return Ok(true);
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

pub(crate) fn remove_api_key_unlocked(
    name: &str,
    is_connector: bool,
    env_keys: Option<&[String]>,
) -> Result<()> {
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
        if let Some(v) = env
            .get(k)
            .filter(|value| api_key_value_is_usable(provider, value))
        {
            return Some(v.clone());
        }
    }
    for k in &keys {
        if let Some(v) = dotenv
            .get(k)
            .filter(|value| api_key_value_is_usable(provider, value))
        {
            return Some(v.clone());
        }
    }
    None
}

pub(crate) fn api_key_value_is_usable(provider: &ProviderConfig, value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    let is_openai_api = Url::parse(&provider.base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "api.openai.com");
    // Codex desktop/frontend OAuth tokens are not OpenAI Platform API keys.
    // Treating one as BYOK causes a predictable 401 from /v1/responses.
    !(is_openai_api && value.starts_with("fe_oa_"))
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
    if let Some(v) = std::env::var(key).ok().filter(|v| !v.is_empty()) {
        return Ok(Some(v));
    }
    // OMGB_API_KEY is a transient input for `omgb provider add` and must not be
    // persisted in ~/.omgb/.env; only the live process environment may supply it.
    if key == "OMGB_API_KEY" {
        return Ok(None);
    }
    let dotenv = load_env_file()?;
    Ok(dotenv.get(key).filter(|v| !v.is_empty()).cloned())
}

pub fn list_providers() -> Result<Vec<ProviderConfig>> {
    let cfg = load_omg_config()?;
    effective_providers_from_tables(&cfg, &load_grok_config_table()?)
}

pub fn get_provider(id: &str) -> Result<Option<ProviderConfig>> {
    let cfg = load_omg_config()?;
    if let Some(provider) = cfg.providers.get(id) {
        return Ok(Some(provider.clone()));
    }
    provider_from_grok_config(id)
}

fn provider_execution_fingerprint_unlocked(model: &str) -> Result<Option<String>> {
    let Some(id) = model.trim().strip_prefix("omgb-") else {
        return Ok(None);
    };
    let Some(provider) = get_provider(id)? else {
        bail!("provider '{id}' is not configured");
    };
    let mut hash = blake3::Hasher::new();
    for value in [
        provider.id.as_str(),
        provider.model.as_str(),
        provider.base_url.as_str(),
        provider.api_backend.as_deref().unwrap_or(""),
    ] {
        hash.update(value.as_bytes());
        hash.update(b"\0");
    }
    hash.update(if provider.no_auth {
        b"no_auth\0"
    } else {
        b"auth\0"
    });
    for value in [
        provider.context_window.map(|value| value.to_string()),
        provider
            .auto_compact_threshold_percent
            .map(|value| value.to_string()),
        provider
            .temperature
            .map(|value| value.to_bits().to_string()),
        provider.top_p.map(|value| value.to_bits().to_string()),
        provider
            .max_completion_tokens
            .map(|value| value.to_string()),
    ] {
        hash.update(value.as_deref().unwrap_or("").as_bytes());
        hash.update(b"\0");
    }
    let mut env_keys = provider.env_key.unwrap_or_default();
    env_keys.sort();
    for key in env_keys {
        hash.update(key.as_bytes());
        hash.update(b"\0");
    }
    let mut headers = provider
        .extra_headers
        .unwrap_or_default()
        .into_iter()
        .collect::<Vec<_>>();
    headers.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, value) in headers {
        hash.update(name.as_bytes());
        hash.update(b"\0");
        hash.update(value.as_bytes());
        hash.update(b"\0");
    }
    Ok(Some(hash.finalize().to_hex().to_string()))
}

pub(crate) fn provider_execution_fingerprint(model: &str) -> Result<Option<String>> {
    if !model.trim().starts_with("omgb-") {
        return Ok(None);
    }
    let _guard = provider_mutation_lock()?;
    provider_execution_fingerprint_unlocked(model)
}

/// Prepare one immutable provider execution window. The mutation locks stay
/// held until the returned guard drops, so config.json, .env and Grok's model
/// table cannot change between identity validation and the model request.
pub(crate) fn prepare_provider_execution(
    model: &str,
    expected_fingerprint: Option<&str>,
) -> Result<Option<ProviderMutationGuard>> {
    let Some(id) = model.trim().strip_prefix("omgb-") else {
        if expected_fingerprint.is_some() {
            bail!("persisted provider identity does not match model '{model}'");
        }
        return Ok(None);
    };
    let guard = provider_mutation_lock()?;
    ensure_provider_configured_unlocked(id)?;
    let current = provider_execution_fingerprint_unlocked(model)?
        .with_context(|| format!("provider '{id}' has no execution fingerprint"))?;
    if expected_fingerprint.is_some_and(|expected| expected != current) {
        bail!("provider '{id}' changed after this execution was planned");
    }
    Ok(Some(guard))
}

fn effective_providers_from_tables(
    cfg: &OmgConfig,
    grok: &toml::map::Map<String, toml::Value>,
) -> Result<Vec<ProviderConfig>> {
    let mut providers = cfg.providers.clone();
    if let Some(models) = grok.get("model").and_then(toml::Value::as_table) {
        for key in models.keys() {
            let Some(id) = key.strip_prefix("omgb-") else {
                continue;
            };
            if id.is_empty() || sanitize_provider_id(id) != id {
                bail!("model.{key} has an invalid provider id");
            }
            if !providers.contains_key(id) {
                let provider = provider_from_grok_table(id, grok)?.with_context(|| {
                    format!("model.{key} disappeared while loading provider configuration")
                })?;
                providers.insert(id.to_string(), provider);
            }
        }
    }
    let mut providers: Vec<_> = providers.into_values().collect();
    providers.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(providers)
}

fn provider_from_grok_table(
    id: &str,
    config: &toml::map::Map<String, toml::Value>,
) -> Result<Option<ProviderConfig>> {
    let key = format!("omgb-{id}");
    let Some(section) = config
        .get("model")
        .and_then(toml::Value::as_table)
        .and_then(|models| models.get(&key))
        .and_then(toml::Value::as_table)
    else {
        return Ok(None);
    };
    let required_string = |field: &str| -> Result<String> {
        section
            .get(field)
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .with_context(|| format!("model.{key}.{field} must be a non-empty string"))
    };
    let optional_u64 = |field: &str| -> Result<Option<u64>> {
        section
            .get(field)
            .map(|value| {
                value
                    .as_integer()
                    .filter(|value| *value >= 0)
                    .map(|value| value as u64)
                    .with_context(|| format!("model.{key}.{field} must be a non-negative integer"))
            })
            .transpose()
    };
    let optional_f64 = |field: &str| -> Result<Option<f64>> {
        section
            .get(field)
            .map(|value| {
                value
                    .as_float()
                    .or_else(|| value.as_integer().map(|value| value as f64))
                    .with_context(|| format!("model.{key}.{field} must be a number"))
            })
            .transpose()
    };
    let env_key = match section.get("env_key") {
        None => None,
        Some(toml::Value::String(value)) if is_valid_env_key(value) => Some(vec![value.clone()]),
        Some(toml::Value::Array(values)) => {
            let mut keys = Vec::with_capacity(values.len());
            for value in values {
                let key_name = value
                    .as_str()
                    .filter(|value| is_valid_env_key(value))
                    .with_context(|| format!("model.{key}.env_key contains an invalid key name"))?;
                keys.push(key_name.to_string());
            }
            Some(keys)
        }
        Some(_) => bail!("model.{key}.env_key must be a valid string or string array"),
    };
    let no_auth = match section.get("auth_scheme") {
        None => false,
        Some(toml::Value::String(value)) if value == "none" => true,
        Some(toml::Value::String(value)) if value == "bearer" || value == "x_api_key" => false,
        Some(toml::Value::String(_)) => {
            bail!("model.{key}.auth_scheme must be bearer, x_api_key, or none")
        }
        Some(_) => bail!("model.{key}.auth_scheme must be a string"),
    };
    let extra_headers = match section.get("extra_headers") {
        None => None,
        Some(toml::Value::Table(headers)) => {
            let mut parsed = HashMap::with_capacity(headers.len());
            for (name, value) in headers {
                let value = value.as_str().with_context(|| {
                    format!("model.{key}.extra_headers.{name} must be a string")
                })?;
                parsed.insert(name.clone(), value.to_string());
            }
            Some(parsed)
        }
        Some(_) => bail!("model.{key}.extra_headers must be a table"),
    };
    let threshold = optional_u64("auto_compact_threshold_percent")?
        .map(|value| {
            u8::try_from(value)
                .ok()
                .filter(|value| *value <= 100)
                .with_context(|| {
                    format!("model.{key}.auto_compact_threshold_percent must be between 0 and 100")
                })
        })
        .transpose()?;

    Ok(Some(ProviderConfig {
        id: id.to_string(),
        name: section
            .get("name")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(id)
            .to_string(),
        model: required_string("model")?,
        base_url: required_string("base_url")?,
        api_backend: section
            .get("api_backend")
            .and_then(toml::Value::as_str)
            .map(str::to_string),
        env_key,
        no_auth,
        extra_headers,
        context_window: optional_u64("context_window")?,
        auto_compact_threshold_percent: threshold,
        temperature: optional_f64("temperature")?,
        top_p: optional_f64("top_p")?,
        max_completion_tokens: optional_u64("max_completion_tokens")?,
    }))
}

fn provider_from_grok_config(id: &str) -> Result<Option<ProviderConfig>> {
    provider_from_grok_table(id, &load_grok_config_table()?)
}

/// If `id` is already in `~/.omgb/config.json`, return it. Otherwise try to
/// materialise a known built-in or catalog template, persist it, and sync it to
/// `~/.grok/config.toml` so upstream model resolution can use `omgb-{id}`.
pub fn ensure_provider_configured(id: &str) -> Result<ProviderConfig> {
    let _lock = provider_mutation_lock()?;
    ensure_provider_configured_unlocked(id)
}

fn ensure_provider_configured_unlocked(id: &str) -> Result<ProviderConfig> {
    let id = sanitize_provider_id(id);
    let mut cfg = load_omg_config()?;
    if let Some(p) = cfg.providers.get(&id).cloned() {
        if p.model.trim().is_empty() {
            bail!("provider '{id}' has no configured model; pass --model or discover local models");
        }
        // `no_auth` is an explicit, security-sensitive boundary. It is set
        // only after add/discovery successfully probes a loopback endpoint,
        // or loaded from an explicit `auth_scheme = "none"` Grok entry. A
        // temporarily missing key must never downgrade an existing provider.
        sync_provider_to_grok_config_unlocked(&p)?;
        return Ok(p);
    }
    if let Some(provider) = provider_from_grok_config(&id)? {
        sync_provider_to_grok_config_unlocked(&provider)?;
        return Ok(provider);
    }
    let provider = provider_template(&id).ok_or_else(|| {
        anyhow::anyhow!("provider '{id}' is not configured and has no known template")
    })?;
    if provider.model.trim().is_empty() {
        bail!("provider '{id}' has no configured model; pass --model or discover local models");
    }
    cfg.providers.insert(id.clone(), provider.clone());
    save_omg_config_unlocked(&cfg)?;
    sync_provider_to_grok_config_unlocked(&provider)?;
    Ok(provider)
}

pub fn remove_provider(id: &str) -> Result<()> {
    let _lock = provider_mutation_lock()?;
    let mut cfg = load_omg_config()?;
    let mut gcfg = load_grok_config_table()?;
    let provider = match cfg.providers.remove(id) {
        Some(provider) => Some(provider),
        None => provider_from_grok_table(id, &gcfg)?,
    };
    if cfg.default_model.as_deref() == Some(&format!("omgb-{id}")) {
        cfg.default_model = None;
    }
    remove_provider_from_grok_table(id, &mut gcfg);
    save_grok_config_table_unlocked(&gcfg)?;
    save_omg_config_unlocked(&cfg)?;
    remove_api_key_unlocked(
        id,
        false,
        provider.as_ref().and_then(|p| p.env_key.as_deref()),
    )?;
    Ok(())
}

pub(crate) fn provider_template(id: &str) -> Option<ProviderConfig> {
    catalog::provider_template(id)
}

fn model_alias_to_provider(model: &str) -> Option<&'static str> {
    match model.to_ascii_lowercase().as_str() {
        "grok" | "grok-3" | "grok3" | "grok-4" | "grok4" | "grok-4.5" | "grok4.5" => Some("xai"),
        "gpt" | "gpt-4" | "gpt4" | "gpt-4o" | "gpt4o" | "gpt-4o-mini" | "gpt4omini"
        | "gpt-4-turbo" | "gpt4turbo" => Some("openai"),
        "claude"
        | "claude-3"
        | "claude3"
        | "claude-3-5"
        | "claude3.5"
        | "claude-3-5-sonnet"
        | "claude-3.5-sonnet"
        | "claude-3-5-sonnet-20241022" => Some("anthropic"),
        "llama"
        | "llama-3"
        | "llama3"
        | "llama-3.3"
        | "llama-3.3-70b"
        | "llama-3.3-70b-versatile" => Some("groq"),
        "gemini" | "gemini-1.5" | "gemini-1.5-flash" => Some("gemini"),
        _ => None,
    }
}

pub(crate) fn resolve_model_to_provider(model: &str) -> Option<String> {
    let m = model.trim();
    if m.is_empty() {
        return None;
    }
    // Provider id/template id takes precedence so exact ids (e.g. "openai")
    // are never misinterpreted.
    let id = sanitize_provider_id(m);
    if get_provider(&id).ok().flatten().is_some() || provider_template(&id).is_some() {
        return Some(id);
    }
    // Canonical aliases come next; they resolve ambiguous model strings
    // (e.g. "gpt-4o" or "claude-3-5-sonnet-20241022") to the canonical
    // provider instead of whichever third-party template happens to be first
    // in the catalog.
    if let Some(alias) = model_alias_to_provider(m) {
        return Some(alias.into());
    }
    // Last resort: exact template model string match.
    for t in catalog::TEMPLATES {
        if t.model.eq_ignore_ascii_case(m) {
            return Some(t.id.into());
        }
    }
    None
}

pub async fn add_provider(args: &AddProviderArgs) -> Result<ProviderConfig> {
    let id = sanitize_provider_id(&args.id);
    if id.is_empty() {
        bail!("provider id is required");
    }

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
            no_auth: false,
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
    if provider.model.trim().is_empty() || provider.model == "local-model" {
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
    let allow_local =
        is_local_provider_id(&provider.id) || is_url_host_loopback(&provider.base_url);
    let allow_private = is_url_host_private(&provider.base_url).await;
    // Reject insecure public HTTP before the provider is saved.
    validate_url(&provider.base_url, allow_local, allow_private).await?;
    provider.no_auth = is_url_host_loopback(&provider.base_url) && api_key_for_fetch.is_none();
    let is_ollama = provider.id == "ollama" || is_ollama_url(&provider.base_url);
    let models = fetch_model_list(
        &provider.base_url,
        api_key_for_fetch.as_deref(),
        backend,
        &extra,
        allow_local,
        allow_private,
        Duration::from_secs(10),
    )
    .await
    .ok_or_else(|| {
        anyhow::anyhow!(
            "could not reach provider at {} or /models probe failed; check the URL, API key, and network",
            provider.base_url
        )
    })?;

    if provider.context_window.is_none() {
        let model_in_list = models.iter().find(|m| m.id == provider.model);
        let cw = if let Some(cw) = model_in_list.and_then(|m| m.context_window) {
            Some(cw)
        } else {
            fetch_model_context_window(
                &provider.base_url,
                api_key_for_fetch.as_deref(),
                backend,
                &extra,
                allow_local,
                allow_private,
                is_ollama,
                &provider.model,
            )
            .await
        };
        provider.context_window = cw
            .or_else(|| fallback_context_window(&provider.model))
            .or(Some(DEFAULT_CONTEXT_WINDOW));
    }
    if provider.auto_compact_threshold_percent.is_none() {
        provider.auto_compact_threshold_percent = Some(80);
    }

    let _lock = provider_mutation_lock()?;
    let mut cfg = load_omg_config()?;
    if args.default || cfg.default_model.is_none() {
        cfg.default_model = Some(format!("omgb-{id}"));
    }
    cfg.providers.insert(id.clone(), provider.clone());
    save_omg_config_unlocked(&cfg)?;
    sync_provider_to_grok_config_unlocked(&provider)?;
    if args.default {
        set_grok_default_model_unlocked(&format!("omgb-{id}"))?;
    }

    // Persist the API key only after the provider config has been saved. This
    // avoids leaving orphaned secrets in ~/.omgb/.env if validation fails.
    if let Some(key) = api_key {
        write_api_key_unlocked(&id, provider.env_key.as_deref(), &key)?;
    }

    Ok(provider)
}

pub fn set_default_provider(id: &str) -> Result<()> {
    let _lock = provider_mutation_lock()?;
    let cfg = load_omg_config()?;
    let provider = match cfg.providers.get(id).cloned() {
        Some(provider) => provider,
        None => provider_from_grok_config(id)?
            .ok_or_else(|| anyhow::anyhow!("provider '{id}' not found"))?,
    };
    sync_provider_to_grok_config_unlocked(&provider)?;
    set_grok_default_model_unlocked(&format!("omgb-{id}"))
}

pub fn set_grok_default_model(model: &str) -> Result<()> {
    let model = model.trim();
    if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        bail!("invalid model identifier");
    }

    let _lock = provider_mutation_lock()?;
    set_grok_default_model_unlocked(model)
}

fn set_grok_default_model_unlocked(model: &str) -> Result<()> {
    let mut gcfg = load_grok_config_table()?;
    let models = gcfg
        .entry("models")
        .or_insert(toml::Value::Table(toml::map::Map::new()));
    if let toml::Value::Table(m) = models {
        m.insert(
            "default".to_string(),
            toml::Value::String(model.to_string()),
        );
    }
    save_grok_config_table_unlocked(&gcfg)?;

    let mut cfg = load_omg_config()?;
    cfg.default_model = model.starts_with("omgb-").then(|| model.to_string());
    save_omg_config_unlocked(&cfg)?;
    Ok(())
}

pub fn configured_default_model() -> Result<Option<String>> {
    let gcfg = load_grok_config_table()?;
    if let Some(default) = gcfg
        .get("models")
        .and_then(toml::Value::as_table)
        .and_then(|models| models.get("default"))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        return Ok(Some(default.to_string()));
    }
    Ok(load_omg_config()?.default_model)
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
    if provider.no_auth {
        section.insert("auth_scheme".into(), toml::Value::String("none".into()));
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

fn sync_provider_to_grok_config_unlocked(provider: &ProviderConfig) -> Result<()> {
    let mut gcfg = load_grok_config_table()?;
    apply_provider_to_grok_table(provider, &mut gcfg);
    set_grok_default_if_unset(&mut gcfg, &format!("omgb-{}", provider.id));
    save_grok_config_table_unlocked(&gcfg)?;
    Ok(())
}

fn remove_provider_from_grok_table(id: &str, gcfg: &mut toml::map::Map<String, toml::Value>) {
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

fn save_grok_config_table_unlocked(table: &toml::map::Map<String, toml::Value>) -> Result<()> {
    let path = grok_config_path();
    let raw = toml::to_string_pretty(&toml::Value::Table(table.clone()))?;
    write_file_atomic(&path, raw, true)
}

async fn discover_one(base_url: &str, name: &str) -> Option<(String, String, Vec<ModelListEntry>)> {
    let models = fetch_model_list(
        base_url,
        None,
        "chat_completions",
        &HashMap::new(),
        true,
        true,
        Duration::from_secs(10),
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
    let vllm = args.vllm_url.as_deref().unwrap_or(DEFAULT_VLLM_URL);
    let sglang = args.sglang_url.as_deref().unwrap_or(DEFAULT_SGLANG_URL);
    let llama_cpp = args
        .llama_cpp_url
        .as_deref()
        .unwrap_or(DEFAULT_LLAMA_CPP_URL);

    let (ollama, lmstudio, vllm, sglang, llama_cpp) = tokio::join!(
        discover_one(ollama, "ollama"),
        discover_one(lmstudio, "lmstudio"),
        discover_one(vllm, "vllm"),
        discover_one(sglang, "sglang"),
        discover_one(llama_cpp, "llama-cpp"),
    );

    Ok([ollama, lmstudio, vllm, sglang, llama_cpp]
        .into_iter()
        .flatten()
        .collect())
}

pub fn add_discovered_providers(
    discovered: &[(String, String, Vec<ModelListEntry>)],
) -> Result<()> {
    let _lock = provider_mutation_lock()?;
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
                no_auth: true,
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
    save_omg_config_unlocked(&cfg)?;

    let mut gcfg = load_grok_config_table()?;
    for p in cfg.providers.values() {
        apply_provider_to_grok_table(p, &mut gcfg);
    }
    if let Some(default) = cfg.default_model.as_deref() {
        set_grok_default_if_unset(&mut gcfg, default);
    }
    save_grok_config_table_unlocked(&gcfg)?;
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

pub(crate) async fn fetch_model_list(
    base_url: &str,
    api_key: Option<&str>,
    backend: &str,
    extra_headers: &HashMap<String, String>,
    allow_local: bool,
    allow_private: bool,
    timeout: Duration,
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

    let text = http_get_text(&url, Some(&headers), timeout).await.ok()?;
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
        Duration::from_secs(10),
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

pub(crate) async fn is_provider_reachable(provider: &ProviderConfig) -> bool {
    if provider.model.trim().is_empty() {
        return false;
    }
    let api_key = resolve_api_key(provider).ok().flatten();
    let backend = provider
        .api_backend
        .as_deref()
        .unwrap_or("chat_completions");
    let extra = provider.extra_headers.clone().unwrap_or_default();
    let allow_local =
        is_local_provider_id(&provider.id) || crate::net::is_url_host_loopback(&provider.base_url);
    let allow_private = crate::net::is_url_host_private(&provider.base_url).await;
    fetch_model_list(
        &provider.base_url,
        api_key.as_deref(),
        backend,
        &extra,
        allow_local,
        allow_private,
        Duration::from_secs(2),
    )
    .await
    .is_some_and(|v| !v.is_empty())
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

    let allow_local =
        is_local_provider_id(&provider.id) || is_url_host_loopback(&provider.base_url);
    let allow_private = is_url_host_private(&provider.base_url).await;

    if let Some(models) = fetch_model_list(
        &base_url,
        api_key.as_deref(),
        backend,
        &headers,
        allow_local,
        allow_private,
        Duration::from_secs(10),
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
    fn conditional_atomic_write_publishes_only_over_expected_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.rs");
        std::fs::write(&path, b"original").unwrap();
        write_file_atomic_if_unchanged(&path, b"original", b"updated").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"updated");

        let error = write_file_atomic_if_unchanged(&path, b"stale", b"clobber").unwrap_err();
        assert!(error.to_string().contains("changed during atomic publish"));
        assert_eq!(std::fs::read(&path).unwrap(), b"updated");
    }

    #[test]
    fn test_sanitize_provider_id() {
        assert_eq!(sanitize_provider_id("OpenAI"), "openai");
        assert_eq!(sanitize_provider_id("my provider!"), "my-provider");
        assert_eq!(sanitize_provider_id("-llama-cpp-"), "llama-cpp");
        assert_eq!(sanitize_provider_id("café"), "caf");
        assert!(sanitize_provider_id("---").is_empty());
    }

    #[test]
    fn effective_provider_loads_from_grok_model_table() {
        let value: toml::Value = toml::from_str(
            r#"
                [model.omgb-localtest]
                name = "Local Runtime Test"
                model = "runtime-test-model"
                base_url = "http://127.0.0.1:55478/v1"
                api_backend = "chat_completions"
                env_key = "OMGB_LOCALTEST_API_KEY"
                context_window = 128000
                auto_compact_threshold_percent = 80
                temperature = 0.2
                top_p = 0.9
                max_completion_tokens = 4096

                [model.omgb-localtest.extra_headers]
                X-Test = "safe-value"
            "#,
        )
        .unwrap();
        let provider = provider_from_grok_table("localtest", value.as_table().unwrap())
            .unwrap()
            .unwrap();

        assert_eq!(provider.id, "localtest");
        assert_eq!(provider.model, "runtime-test-model");
        assert_eq!(provider.base_url, "http://127.0.0.1:55478/v1");
        assert_eq!(
            provider.env_key,
            Some(vec!["OMGB_LOCALTEST_API_KEY".into()])
        );
        assert_eq!(provider.context_window, Some(128_000));
        assert_eq!(provider.auto_compact_threshold_percent, Some(80));
        assert_eq!(provider.temperature, Some(0.2));
        assert_eq!(provider.top_p, Some(0.9));
        assert_eq!(provider.max_completion_tokens, Some(4096));
        assert_eq!(
            provider
                .extra_headers
                .as_ref()
                .and_then(|headers| headers.get("X-Test"))
                .map(String::as_str),
            Some("safe-value")
        );
    }

    #[test]
    fn effective_provider_list_merges_grok_models_and_prefers_omg_config() {
        let grok: toml::Value = toml::from_str(
            r#"
                [model.omgb-shared]
                name = "Grok copy"
                model = "stale-model"
                base_url = "https://stale.example/v1"

                [model.omgb-zebra]
                name = "Zebra"
                model = "zebra-model"
                base_url = "https://zebra.example/v1"
            "#,
        )
        .unwrap();
        let explicit = ProviderConfig {
            id: "shared".into(),
            name: "Explicit copy".into(),
            model: "current-model".into(),
            base_url: "https://current.example/v1".into(),
            api_backend: None,
            env_key: None,
            no_auth: false,
            extra_headers: None,
            context_window: None,
            auto_compact_threshold_percent: None,
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        };
        let cfg = OmgConfig {
            providers: HashMap::from([("shared".into(), explicit)]),
            ..OmgConfig::default()
        };

        let providers = effective_providers_from_tables(&cfg, grok.as_table().unwrap()).unwrap();

        assert_eq!(
            providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec!["shared", "zebra"]
        );
        assert_eq!(providers[0].model, "current-model");
        assert_eq!(providers[1].model, "zebra-model");
    }

    #[test]
    fn effective_provider_list_rejects_invalid_grok_provider_id() {
        let grok: toml::Value = toml::from_str(
            r#"
                [model."omgb-Bad Id"]
                model = "test"
                base_url = "https://example.com/v1"
            "#,
        )
        .unwrap();

        assert!(
            effective_providers_from_tables(&OmgConfig::default(), grok.as_table().unwrap())
                .is_err()
        );
    }

    #[test]
    fn effective_provider_rejects_unsafe_env_key() {
        let value: toml::Value = toml::from_str(
            r#"
                [model.omgb-bad]
                model = "test"
                base_url = "http://127.0.0.1:1/v1"
                env_key = "PATH"
            "#,
        )
        .unwrap();

        assert!(provider_from_grok_table("bad", value.as_table().unwrap()).is_err());
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
            no_auth: false,
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
    fn openai_provider_rejects_frontend_oauth_token_as_api_key() {
        let provider = ProviderConfig {
            id: "codex".into(),
            name: "OpenAI Codex".into(),
            model: "codex-mini-latest".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_backend: Some("responses".into()),
            env_key: Some(vec!["OPENAI_API_KEY".into()]),
            no_auth: false,
            extra_headers: None,
            context_window: None,
            auto_compact_threshold_percent: None,
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        };
        let env = HashMap::from([("OPENAI_API_KEY".into(), "fe_oa_not_a_platform_key".into())]);

        assert_eq!(
            resolve_api_key_with_maps(&provider, &env, &HashMap::new()),
            None
        );
        assert!(api_key_value_is_usable(&provider, "sk-proj-valid-shape"));
    }

    #[test]
    fn concurrent_api_key_writes_preserve_every_entry() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let tmp =
            std::env::temp_dir().join(format!("omgb-provider-env-test-{}", uuid::Uuid::new_v4()));
        set_omg_home_for_tests(Some(tmp.clone()));

        let writers: Vec<_> = (0..8)
            .map(|index| {
                std::thread::spawn(move || {
                    write_api_key(
                        &format!("provider-{index}"),
                        None,
                        &format!("secret-{index}"),
                    )
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }

        let entries = load_env_file().unwrap();
        for index in 0..8 {
            assert_eq!(
                entries.get(&format!("OMGB_PROVIDER_{index}_API_KEY")),
                Some(&format!("secret-{index}"))
            );
        }

        set_omg_home_for_tests(None);
        let _ = std::fs::remove_dir_all(tmp);
    }

    fn test_provider(id: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            name: id.into(),
            model: "test-model".into(),
            base_url: "https://example.com/v1".into(),
            api_backend: None,
            env_key: None,
            no_auth: false,
            extra_headers: None,
            context_window: Some(128_000),
            auto_compact_threshold_percent: Some(80),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        }
    }

    #[test]
    fn ensure_provider_repairs_a_missing_grok_entry() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "omgb-provider-reconcile-test-{}",
            uuid::Uuid::new_v4()
        ));
        let omg = root.join("omg");
        let grok = root.join("grok");
        set_omg_home_for_tests(Some(omg));
        set_grok_home_for_tests(Some(grok));
        let mut provider = test_provider("repair");
        provider.no_auth = true;
        save_omg_config(&OmgConfig {
            default_model: Some("omgb-repair".into()),
            providers: HashMap::from([("repair".into(), provider)]),
            relay: None,
        })
        .unwrap();

        ensure_provider_configured("repair").unwrap();
        assert!(
            provider_from_grok_config("repair")
                .unwrap()
                .unwrap()
                .no_auth
        );

        set_grok_home_for_tests(None);
        set_omg_home_for_tests(None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ensure_provider_never_downgrades_missing_loopback_credentials() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "omgb-provider-auth-boundary-test-{}",
            uuid::Uuid::new_v4()
        ));
        set_omg_home_for_tests(Some(root.join("omg")));
        set_grok_home_for_tests(Some(root.join("grok")));
        let mut provider = test_provider("keyed-local");
        provider.base_url = "http://127.0.0.1:12345/v1".into();
        provider.env_key = Some(vec!["OMGB_KEYED_LOCAL_API_KEY".into()]);
        save_omg_config(&OmgConfig {
            default_model: Some("omgb-keyed-local".into()),
            providers: HashMap::from([("keyed-local".into(), provider)]),
            relay: None,
        })
        .unwrap();

        let configured = ensure_provider_configured("keyed-local").unwrap();
        assert!(!configured.no_auth);
        let grok_provider = provider_from_grok_config("keyed-local").unwrap().unwrap();
        assert!(!grok_provider.no_auth);

        set_grok_home_for_tests(None);
        set_omg_home_for_tests(None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn set_default_provider_replaces_the_existing_grok_default() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "omgb-provider-default-test-{}",
            uuid::Uuid::new_v4()
        ));
        set_omg_home_for_tests(Some(root.join("omg")));
        set_grok_home_for_tests(Some(root.join("grok")));
        save_omg_config(&OmgConfig {
            default_model: None,
            providers: HashMap::from([("next".into(), test_provider("next"))]),
            relay: None,
        })
        .unwrap();
        let mut grok = toml::map::Map::new();
        let mut models = toml::map::Map::new();
        models.insert("default".into(), toml::Value::String("old-model".into()));
        grok.insert("models".into(), toml::Value::Table(models));
        save_grok_config_table_unlocked(&grok).unwrap();

        set_default_provider("next").unwrap();
        assert_eq!(
            configured_default_model().unwrap().as_deref(),
            Some("omgb-next")
        );
        assert!(provider_from_grok_config("next").unwrap().is_some());

        set_grok_home_for_tests(None);
        set_omg_home_for_tests(None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn provider_execution_fingerprint_changes_with_runtime_configuration() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "omgb-provider-fingerprint-test-{}",
            uuid::Uuid::new_v4()
        ));
        set_omg_home_for_tests(Some(root.join("omg")));
        set_grok_home_for_tests(Some(root.join("grok")));
        let mut provider = test_provider("fingerprint");
        save_omg_config(&OmgConfig {
            default_model: None,
            providers: HashMap::from([("fingerprint".into(), provider.clone())]),
            relay: None,
        })
        .unwrap();
        let before = provider_execution_fingerprint("omgb-fingerprint")
            .unwrap()
            .unwrap();
        provider.base_url = "https://changed.example/v1".into();
        save_omg_config(&OmgConfig {
            default_model: None,
            providers: HashMap::from([("fingerprint".into(), provider)]),
            relay: None,
        })
        .unwrap();
        let after = provider_execution_fingerprint("omgb-fingerprint")
            .unwrap()
            .unwrap();
        assert_ne!(before, after);
        assert!(
            prepare_provider_execution("omgb-fingerprint", Some(&before)).is_err(),
            "a planned turn must reject a provider changed before execution"
        );

        set_grok_home_for_tests(None);
        set_omg_home_for_tests(None);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn test_ensure_provider_configured_rejects_empty_model() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let tmp =
            std::env::temp_dir().join(format!("omgb-providers-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        crate::providers::set_omg_home_for_tests(Some(tmp.clone()));

        let provider = ProviderConfig {
            id: "emptymodel".into(),
            name: "Empty".into(),
            model: String::new(),
            base_url: "https://example.com/v1".into(),
            api_backend: None,
            env_key: None,
            no_auth: false,
            extra_headers: None,
            context_window: None,
            auto_compact_threshold_percent: None,
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
        };
        let cfg = OmgConfig {
            default_model: None,
            providers: [("emptymodel".into(), provider)]
                .into_iter()
                .collect::<HashMap<_, _>>(),
            relay: None,
        };
        save_omg_config(&cfg).unwrap();

        let err = ensure_provider_configured("emptymodel").unwrap_err();
        assert!(err.to_string().contains("no configured model"));
        crate::providers::set_omg_home_for_tests(None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(windows)]
    #[test]
    fn restricted_atomic_rewrite_replaces_a_broad_windows_acl() {
        let root =
            std::env::temp_dir().join(format!("omgb-private-file-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("secret.env");
        std::fs::write(&file, b"TOKEN=secret\n").unwrap();

        assert!(
            windows_permissions_restriction_issue(&file)
                .unwrap()
                .is_some()
        );
        write_file_atomic(&file, b"TOKEN=updated\n", true).unwrap();
        assert_eq!(windows_permissions_restriction_issue(&file).unwrap(), None);
        assert_eq!(std::fs::read(&file).unwrap(), b"TOKEN=updated\n");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn restricted_directory_keeps_existing_and_new_children_accessible() {
        let root = std::env::temp_dir().join(format!(
            "omgb-private-directory-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let existing = root.join("existing.lock");
        std::fs::write(&existing, b"before").unwrap();

        restrict_omg_directory_permissions(&root).unwrap();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&existing)
            .unwrap();
        let new_child = root.join("new.lock");
        std::fs::write(&new_child, b"after").unwrap();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&new_child)
            .unwrap();

        std::fs::remove_dir_all(root).unwrap();
    }
}
