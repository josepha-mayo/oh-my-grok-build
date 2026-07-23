//! Persistent recorded-approval store for the auto-mode permission classifier.
//!
//! Approved tool patterns are written to `~/.omgb/approvals.jsonl` with a TTL.
//! When `omgb` runs in `auto` permission mode, unexpired approvals are loaded as
//! `allow_rules` so previously-granted tool calls are honored instead of silently
//! denied.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

const DEFAULT_APPROVAL_TTL_DAYS: i64 = 30;
const MAX_APPROVALS: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct Approval {
    pub tool: String,
    pub pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

fn approvals_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("approvals.jsonl"))
}

fn ttl() -> Duration {
    let days = std::env::var("OMGB_APPROVAL_TTL_DAYS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(DEFAULT_APPROVAL_TTL_DAYS);
    Duration::days(days.max(1))
}

pub fn load_approvals(now: DateTime<Utc>) -> Result<Vec<Approval>> {
    let path = approvals_path()?;
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path).context("read approvals")?;
    let mut approvals = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let approval: Approval = serde_json::from_str(line).context("parse approval")?;
        if approval.expires_at.is_some_and(|exp| exp <= now) {
            continue;
        }
        approvals.push(approval);
    }
    Ok(approvals)
}

pub fn to_allow_rules(approvals: &[Approval]) -> Vec<String> {
    approvals
        .iter()
        .map(|a| {
            if a.pattern.is_empty() {
                a.tool.clone()
            } else {
                format!("{}({})", a.tool, a.pattern)
            }
        })
        .collect()
}

fn normalize_tool_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "run_terminal_cmd" | "bash" | "monitor" => "Bash".to_string(),
        "read_file" | "read" | "notebook_read" => "Read".to_string(),
        "edit_file" | "search_replace" | "apply_patch" | "write" | "notebook_edit"
        | "hashline_edit" => "Edit".to_string(),
        "web_search" => "WebSearch".to_string(),
        "web_fetch" => "WebFetch".to_string(),
        "grep" | "glob" => "Grep".to_string(),
        "mcp" | "mcptool" => "MCPTool".to_string(),
        _ => {
            let mut s = name.to_string();
            if let Some(first) = s.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            s
        }
    }
}

fn extract_pattern(tool: &str, args: &str) -> Option<String> {
    let args = args.trim();
    if args.is_empty() {
        return None;
    }
    let value: serde_json::Value =
        serde_json::from_str(args).unwrap_or(serde_json::Value::String(args.to_string()));
    match tool {
        "Bash" | "Monitor" => value.get("command").and_then(|v| v.as_str()).map(|s| {
            let cmd = s.trim();
            format!("{}:*", cmd)
        }),
        "Read" | "Edit" => value
            .get("file_path")
            .or_else(|| value.get("path"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
        "WebSearch" => value
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
        "WebFetch" => value
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
        "Grep" | "Glob" => value
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
        "MCPTool" => value
            .get("tool_name")
            .or_else(|| value.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
        _ => Some(args.to_string()),
    }
}

pub fn record_tool_calls(calls: &[(String, String)]) -> Result<()> {
    if calls.is_empty() {
        return Ok(());
    }
    let path = approvals_path()?;
    let mut existing: HashSet<Approval> = load_approvals(Utc::now())?.into_iter().collect();
    let ttl = ttl();
    let now = Utc::now();
    for (name, args) in calls {
        let tool = normalize_tool_name(name);
        let Some(pattern) = extract_pattern(&tool, args) else {
            continue;
        };
        existing.insert(Approval {
            tool,
            pattern,
            expires_at: Some(now + ttl),
        });
    }
    let mut approvals: Vec<Approval> = existing.into_iter().collect();
    if approvals.len() > MAX_APPROVALS {
        approvals.sort_by(|a, b| a.expires_at.cmp(&b.expires_at).reverse());
        approvals.truncate(MAX_APPROVALS);
    }
    let mut lines = String::new();
    for a in approvals {
        lines.push_str(&serde_json::to_string(&a).context("serialize approval")?);
        lines.push('\n');
    }
    crate::providers::write_file_atomic(&path, lines, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_pattern_uses_command_with_colon_wildcard() {
        assert_eq!(
            extract_pattern("Bash", r#"{"command":"rm -rf /"}"#),
            Some("rm -rf /:*".into())
        );
        assert_eq!(
            extract_pattern("Bash", r#"{"command":"git status"}"#),
            Some("git status:*".into())
        );
        assert_eq!(
            extract_pattern("Bash", r#"{"command":"rm"}"#),
            Some("rm:*".into())
        );
    }

    #[test]
    fn bash_pattern_has_no_trailing_space() {
        let pattern = extract_pattern("Bash", r#"{"command":"rm -rf /"}"#).unwrap();
        assert!(
            !pattern.ends_with(' '),
            "pattern '{pattern}' should not end with a space"
        );
    }

    #[test]
    fn read_edit_patterns_use_path() {
        assert_eq!(
            extract_pattern("Read", r#"{"file_path":"src/main.rs"}"#),
            Some("src/main.rs".into())
        );
        assert_eq!(
            extract_pattern("Edit", r#"{"path":"/etc/passwd"}"#),
            Some("/etc/passwd".into())
        );
    }

    #[test]
    fn to_allow_rules_wraps_patterns() {
        let approvals = vec![
            Approval {
                tool: "Bash".into(),
                pattern: "rm -rf /:*".into(),
                expires_at: None,
            },
            Approval {
                tool: "Read".into(),
                pattern: "src/main.rs".into(),
                expires_at: None,
            },
        ];
        let rules = to_allow_rules(&approvals);
        assert!(rules.contains(&"Bash(rm -rf /:*)".into()));
        assert!(rules.contains(&"Read(src/main.rs)".into()));
    }

    #[test]
    fn empty_pattern_yields_bare_tool() {
        let approvals = vec![Approval {
            tool: "Bash".into(),
            pattern: "".into(),
            expires_at: None,
        }];
        assert_eq!(to_allow_rules(&approvals), vec!["Bash".to_string()]);
    }
}
