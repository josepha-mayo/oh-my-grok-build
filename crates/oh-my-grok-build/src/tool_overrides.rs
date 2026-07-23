//! Tool override helpers for composing per-prompt or per-agent tool lists.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xai_grok_pager::app::PagerArgs;
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
    if !path.exists() {
        return Ok(());
    }
    let omg = crate::providers::omg_dir()?;
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let canonical = dunce::canonicalize(&abs)?;
    let omg_canonical =
        dunce::canonicalize(&omg).unwrap_or_else(|_| dunce::simplified(&omg).to_path_buf());
    if !canonical.starts_with(&omg_canonical) {
        bail!(
            "tool overrides path must be under {}: {}",
            omg_canonical.display(),
            path.display()
        );
    }
    Ok(())
}

fn merge_file(base: &mut Value, path: &Path) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read tool overrides {}", path.display()))?;
    let value: Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    *base = merge_tool_overrides(std::mem::take(base), value);
    Ok(())
}

/// Load tool overrides from (in order of increasing precedence):
///   1. `~/.omgb/tool_overrides.json`
///   2. the file in `OMGB_TOOL_OVERRIDES`
///   3. `~/.omgb/sessions/{session_id}/tool_overrides.json`
///   4. `~/.omgb/agents/{agent}/tool_overrides.json`
///   5. `~/.omgb/agents/{agent}/agents.json` (sets the `agents_json` key)
///
/// The per-agent files are loaded for the agent named by the `agent` argument
/// or by an `agent` key in an already-loaded override file, so a global
/// override can select and configure an agent in one place.
pub fn load_tool_overrides(agent: Option<&str>, session_id: Option<&str>) -> Result<ToolOverrides> {
    let mut base = Value::Object(serde_json::Map::new());
    let omg = crate::providers::omg_dir()?;

    merge_file(&mut base, &omg.join("tool_overrides.json"))?;

    if let Ok(extra) = std::env::var("OMGB_TOOL_OVERRIDES") {
        let extra = Path::new(&extra);
        ensure_under_omg_dir(extra)?;
        merge_file(&mut base, extra)?;
    }

    if let Some(sid) = session_id
        && validate_agent(sid).is_ok()
    {
        let path = omg.join("sessions").join(sid).join("tool_overrides.json");
        merge_file(&mut base, &path)?;
    }

    let effective_agent = base
        .get("agent")
        .and_then(|v| v.as_str())
        .filter(|a| validate_agent(a).is_ok())
        .or(agent)
        .filter(|a| validate_agent(a).is_ok());

    if let Some(a) = effective_agent {
        let agent_dir = omg.join("agents").join(a);
        merge_file(&mut base, &agent_dir.join("tool_overrides.json"))?;

        let agents_json_path = agent_dir.join("agents.json");
        if agents_json_path.is_file() && base.get("agents_json").is_none() {
            let raw = std::fs::read_to_string(&agents_json_path)
                .with_context(|| format!("failed to read {}", agents_json_path.display()))?;
            base["agents_json"] = Value::String(raw);
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
    if options.agents_json.is_none()
        && let Some(s) = overrides.0.get("agents_json").and_then(|v| v.as_str())
    {
        options.agents_json = Some(s.to_string());
    }
    if options.reasoning_effort.is_none()
        && let Some(s) = overrides.0.get("reasoning_effort").and_then(|v| v.as_str())
    {
        options.reasoning_effort = Some(s.to_string());
    }
    Ok(())
}

/// Apply the same override rules to the TUI `PagerArgs` so the default TUI
/// path picks up per-agent/session tool lists and permission rules.
pub fn apply_tool_overrides_to_pager_args(
    overrides: &ToolOverrides,
    options: &mut PagerArgs,
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
    if options.agents_json.is_none()
        && let Some(s) = overrides.0.get("agents_json").and_then(|v| v.as_str())
    {
        options.agents_json = Some(s.to_string());
    }
    if options.reasoning_effort.is_none()
        && let Some(s) = overrides.0.get("reasoning_effort").and_then(|v| v.as_str())
    {
        options.reasoning_effort = Some(s.to_string());
    }
    Ok(())
}
