//! Tool override helpers for composing per-prompt or per-agent tool lists.

use std::path::Path;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xai_grok_pager::headless::HeadlessOptions;

/// A thin wrapper around a JSON value that carries tool-list overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolOverrides(pub Value);

impl From<Value> for ToolOverrides {
    fn from(v: Value) -> Self {
        Self(v)
    }
}

impl From<ToolOverrides> for Value {
    fn from(t: ToolOverrides) -> Self {
        t.0
    }
}

/// Recursively merge two JSON values. Objects are merged by key; all other
/// values are replaced by `update`.
pub fn merge_tool_overrides(base: Value, update: Value) -> Value {
    match (base, update) {
        (Value::Object(mut base_map), Value::Object(update_map)) => {
            for (k, v) in update_map {
                let entry = base_map.entry(k).or_insert(Value::Null);
                *entry = merge_tool_overrides(entry.take(), v);
            }
            Value::Object(base_map)
        }
        (_, update) => update,
    }
}

fn string_from_value(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }
    value.as_array().map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect::<Vec<_>>()
            .join(",")
    })
}

fn ensure_under_omg_dir(path: &Path) -> Result<()> {
    let omg = crate::providers::omg_dir()?;
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let canonical = dunce::simplified(&abs).to_path_buf();
    let omg_canonical = dunce::simplified(&omg).to_path_buf();
    if !canonical.starts_with(&omg_canonical) {
        bail!(
            "tool overrides path must be under {}: {}",
            omg_canonical.display(),
            path.display()
        );
    }
    Ok(())
}

/// Load tool overrides from `~/.omgb/tool_overrides.json` (if present), merging
/// with the file pointed to by `OMGB_TOOL_OVERRIDES` (if set).
pub fn load_tool_overrides() -> Result<ToolOverrides> {
    let mut base = Value::Object(serde_json::Map::new());
    let default_path = crate::providers::omg_dir()?.join("tool_overrides.json");
    if default_path.is_file() {
        let raw = std::fs::read_to_string(&default_path)?;
        base = merge_tool_overrides(base, serde_json::from_str(&raw)?);
    }
    if let Ok(extra) = std::env::var("OMGB_TOOL_OVERRIDES") {
        let extra = Path::new(&extra);
        ensure_under_omg_dir(extra)?;
        if let Ok(raw) = std::fs::read_to_string(extra) {
            base = merge_tool_overrides(base, serde_json::from_str(&raw)?);
        }
    }
    Ok(ToolOverrides(base))
}

fn validate_tool_list(name: &str, value: &str) -> Result<()> {
    for tool in value.split(',') {
        let tool = tool.trim();
        if tool.is_empty() {
            continue;
        }
        if !tool
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '-'))
        {
            bail!("{name} contains invalid tool name: {tool}");
        }
    }
    Ok(())
}

fn validate_agent(value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("agent must not be empty");
    }
    if value.contains("..") || value.contains('\\') || value.starts_with('/') {
        bail!("agent must be a simple name, not a path: {value}");
    }
    Ok(())
}

/// Apply `overrides` to `HeadlessOptions`. CLI values take precedence over
/// config-file values.
pub fn apply_tool_overrides_to_headless_options(
    overrides: &ToolOverrides,
    options: &mut HeadlessOptions,
) -> Result<()> {
    if options.cli_tools.is_none()
        && let Some(tools) = overrides.0.get("tools").and_then(string_from_value)
        && !tools.is_empty()
    {
        validate_tool_list("tools", &tools)?;
        options.cli_tools = Some(tools);
    }
    if options.cli_disallowed_tools.is_none()
        && let Some(disallowed) = overrides
            .0
            .get("disallowed_tools")
            .and_then(string_from_value)
        && !disallowed.is_empty()
    {
        validate_tool_list("disallowed_tools", &disallowed)?;
        options.cli_disallowed_tools = Some(disallowed);
    }
    if options.max_turns.is_none()
        && let Some(n) = overrides.0.get("max_turns").and_then(|v| v.as_u64())
    {
        options.max_turns = Some(n as u32);
    }
    if options.permission_mode_flag.is_none()
        && let Some(s) = overrides.0.get("permission_mode").and_then(|v| v.as_str())
    {
        options.permission_mode_flag = Some(s.to_string());
    }
    if let Some(arr) = overrides.0.get("allow_rules").and_then(|v| v.as_array()) {
        options
            .allow_rules
            .extend(arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())));
    }
    if let Some(arr) = overrides.0.get("deny_rules").and_then(|v| v.as_array()) {
        options
            .deny_rules
            .extend(arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())));
    }
    if let Some(b) = overrides
        .0
        .get("disable_web_search")
        .and_then(|v| v.as_bool())
    {
        options.disable_web_search = options.disable_web_search || b;
    }
    if options.agent.is_none()
        && let Some(s) = overrides.0.get("agent").and_then(|v| v.as_str())
    {
        validate_agent(s)?;
        options.agent = Some(s.to_string());
    }
    if options.reasoning_effort.is_none()
        && let Some(s) = overrides.0.get("reasoning_effort").and_then(|v| v.as_str())
    {
        options.reasoning_effort = Some(s.to_string());
    }
    Ok(())
}
