//! Tool override helpers for composing per-prompt or per-agent tool lists.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use xai_grok_shell::agent::config::{CliAgentOverrides, Config as AgentConfig};

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

fn parse_string_list(value: &Value) -> Option<Vec<String>> {
    if let Some(arr) = value.as_array() {
        let list: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if !list.is_empty() {
            return Some(list);
        }
    }
    if let Some(s) = value.as_str() {
        let list: Vec<String> = s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !list.is_empty() {
            return Some(list);
        }
    }
    None
}

/// Apply `overrides` to `agent_config.cli_agent_overrides`.
///
/// The JSON object is expected to contain keys such as `tools` and
/// `disallowed_tools`, either as JSON arrays of strings or as comma-separated
/// strings.
pub fn apply_tool_overrides_to_agent_config(
    agent_config: &mut AgentConfig,
    overrides: &ToolOverrides,
) {
    let mut o = CliAgentOverrides::default();
    if let Some(tools) = overrides.0.get("tools").and_then(parse_string_list) {
        o.tools = Some(tools);
    }
    if let Some(disallowed) = overrides
        .0
        .get("disallowed_tools")
        .and_then(parse_string_list)
    {
        o.disallowed_tools = Some(disallowed);
    }
    agent_config.cli_agent_overrides = o;
}
