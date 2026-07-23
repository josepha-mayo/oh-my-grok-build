//! Auto skill creation and retrieval for `omgb`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::prompt_guard;
use crate::timeline::TimelineEvent;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Skill {
    pub name: String,
    pub trigger: String,
    pub steps: Vec<String>,
    pub pitfalls: Vec<String>,
    pub verification: Vec<String>,
    #[serde(skip)]
    pub path: PathBuf,
}

fn skills_dir() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("skills"))
}

fn safe_filename(name: &str) -> String {
    let mut out = String::new();
    let mut prev = '_';
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
            prev = c;
        } else if prev != '_' {
            out.push('_');
            prev = '_';
        }
    }
    let out = out.trim_end_matches('_').to_lowercase();
    if out.is_empty() || out == "_" {
        "untitled".into()
    } else {
        out
    }
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    if !text.starts_with("+++\n") {
        return None;
    }
    let rest = &text[4..];
    let end = rest.find("\n+++")?;
    let front = &rest[..end];
    let after = &rest[end + 4..];
    Some((front, after.strip_prefix('\n').unwrap_or(after)))
}

fn format_list(items: &[String]) -> String {
    if items.is_empty() {
        "- none\n".into()
    } else {
        items.iter().map(|s| format!("- {s}\n")).collect()
    }
}

pub fn format_skill_markdown(skill: &Skill) -> Result<String> {
    let front = toml::to_string(skill)?;
    let body = format!(
        "# {}\n\n> Trigger: `{}`\n\n## Steps\n{}\n## Pitfalls\n{}\n## Verification\n{}\n",
        skill.name,
        skill.trigger,
        format_list(&skill.steps),
        format_list(&skill.pitfalls),
        format_list(&skill.verification)
    );
    Ok(format!("+++\n{front}+++\n\n{body}"))
}

fn parse_skill_markdown(text: &str) -> Result<Skill> {
    let (front, _body) = split_frontmatter(text)
        .ok_or_else(|| anyhow::anyhow!("skill markdown has no TOML frontmatter"))?;
    toml::from_str(front).map_err(|e| anyhow::anyhow!("invalid skill frontmatter: {e}"))
}

/// Write a skill to `~/.omgb/skills/{name}.md`.
pub fn write_skill(skill: &Skill) -> Result<()> {
    let dir = skills_dir()?;
    std::fs::create_dir_all(&dir)?;
    let filename = format!("{}.md", safe_filename(&skill.name));
    let path = dir.join(filename);
    crate::providers::write_file_atomic(&path, format_skill_markdown(skill)?, true)
}

fn plugin_skills_dirs() -> Vec<PathBuf> {
    let plugins_dir = crate::providers::omg_dir().ok().map(|d| d.join("plugins"));
    let Some(plugins_dir) = plugins_dir else {
        return Vec::new();
    };
    if !plugins_dir.is_dir() {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(&plugins_dir).into_iter().flatten() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_dir() {
            let skills = path.join("skills");
            if skills.is_dir() {
                dirs.push(skills);
            }
        }
    }
    dirs
}

fn collect_skills_in_dir(dir: &Path, skills: &mut Vec<Skill>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md") {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            match parse_skill_markdown(&text) {
                Ok(mut skill) => {
                    skill.path = path;
                    skills.push(skill);
                }
                Err(e) => eprintln!("warning: skipping invalid skill {}: {e}", path.display()),
            }
        }
    }
}

/// List all persisted skills, including skills hot-loaded from installed plugins.
pub fn list_skills() -> Result<Vec<Skill>> {
    let mut skills = Vec::new();
    if let Ok(dir) = skills_dir() {
        collect_skills_in_dir(&dir, &mut skills);
    }
    for dir in plugin_skills_dirs() {
        collect_skills_in_dir(&dir, &mut skills);
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

fn path_components(path: &str) -> Vec<String> {
    path.split(['/', '\\'])
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn trigger_matches(cwd: &str, trigger: &str) -> bool {
    if trigger.is_empty() {
        return true;
    }
    let cwd = path_components(cwd);
    let trigger = path_components(trigger);
    if trigger.len() == 1 {
        cwd.iter().any(|c| c == &trigger[0])
    } else {
        cwd.windows(trigger.len()).any(|w| w == trigger.as_slice())
    }
}

/// Return a markdown string of skills whose trigger matches the current directory.
pub fn skill_preamble() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let skills = match list_skills() {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let mut relevant = Vec::new();
    for s in skills
        .into_iter()
        .filter(|s| trigger_matches(&cwd, &s.trigger))
    {
        let Some(name) = prompt_guard::sanitize_inline(&s.name, 120) else {
            continue;
        };
        let body = if s.path.exists() {
            std::fs::read_to_string(&s.path)
                .ok()
                .and_then(|text| split_frontmatter(&text).map(|(_, b)| b.to_string()))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let Some(body) = prompt_guard::sanitize_skill_body(&body, 2000) else {
            continue;
        };
        if !body.is_empty() {
            relevant.push(format!("# {name}\n\n{body}"));
        }
    }
    if relevant.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", relevant.join("\n\n---\n\n"))
    }
}

fn group_runs(events: Vec<TimelineEvent>) -> Vec<Vec<TimelineEvent>> {
    let mut runs: Vec<Vec<TimelineEvent>> = Vec::new();
    for ev in events {
        if ev.category == "exec" || ev.category == "run" || ev.category == "autonomous" {
            runs.push(vec![ev]);
        } else if let Some(last) = runs.last_mut() {
            last.push(ev);
        } else {
            runs.push(vec![ev]);
        }
    }
    runs
}

fn tool_call_count(e: &TimelineEvent) -> usize {
    if e.category == "tool" || e.category == "tool_call" {
        e.data
            .as_ref()
            .and_then(|d| d.get("tool_calls"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize
    } else {
        e.data
            .as_ref()
            .and_then(|d| d.get("tool_calls"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize
    }
}

fn run_has_errors(run: &[TimelineEvent]) -> bool {
    run.iter().any(|e| {
        e.category == "error"
            || e.category == "failure"
            || e.data.as_ref().is_some_and(|d| {
                d.get("error").is_some()
                    || d.get("errors")
                        .and_then(|v| v.as_array())
                        .is_some_and(|a| !a.is_empty())
            })
    })
}

fn run_has_success(run: &[TimelineEvent]) -> bool {
    run.iter().any(|e| {
        e.category == "success"
            || e.category == "done"
            || e.data.as_ref().is_some_and(|d| d.get("success").is_some())
    })
}

fn run_has_user_corrections(run: &[TimelineEvent]) -> bool {
    run.iter().any(|e| e.category == "user_correction")
}

fn summarize_run(run: &[TimelineEvent]) -> String {
    run.iter()
        .map(|e| {
            format!(
                "{} [{}] {}",
                e.timestamp.to_rfc3339(),
                e.category,
                e.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_json(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix("```json")
        .and_then(|s| s.rfind("```").map(|i| &s[..i]))
    {
        inner.trim().to_string()
    } else if let Some(inner) = trimmed
        .strip_prefix("```")
        .and_then(|s| s.rfind("```").map(|i| &s[..i]))
    {
        inner.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

async fn llm_generate(prompt: &str) -> Result<String> {
    let prompt_file = crate::write_prompt_temp(prompt).await?;
    let _guard = crate::PromptFileGuard(prompt_file.clone());
    let exe = std::env::current_exe()?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("exec")
        .arg("--prompt-file")
        .arg(&prompt_file)
        .arg("--yolo")
        .arg("--tools")
        .arg("read_file,grep,list_dir,web_search,web_fetch")
        .env_remove("OMGB_AUTO_SKILL")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let out = match tokio::time::timeout(Duration::from_secs(120), cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => bail!("skill generation subprocess failed: {e}"),
        Err(_) => bail!("skill generation timed out after 120s"),
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("skill generation failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Read `timeline.jsonl` and, if a run had >= `threshold` tool calls and either
/// (errors + eventual success) or explicit user corrections, ask the LLM to
/// generate a reusable `Skill`.
fn run_qualifies_for_skill(run: &[TimelineEvent], threshold: usize) -> bool {
    let tool_calls: usize = run.iter().map(tool_call_count).sum();
    tool_calls >= threshold
        && ((run_has_errors(run) && run_has_success(run)) || run_has_user_corrections(run))
}

pub async fn auto_create_skill_from_timeline(threshold: usize) -> Result<Option<Skill>> {
    let path = crate::providers::omg_dir()?.join("timeline.jsonl");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let mut events: Vec<TimelineEvent> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str(l) {
            Ok(e) => Some(e),
            Err(e) => {
                eprintln!("warning: skipping malformed timeline line: {e}");
                None
            }
        })
        .collect();
    events.sort_by_key(|e| e.timestamp);

    let runs = group_runs(events);
    let mut candidate: Option<&[TimelineEvent]> = None;
    for run in runs.iter().rev() {
        if run_qualifies_for_skill(run, threshold) {
            candidate = Some(run);
            break;
        }
    }

    let Some(run) = candidate else {
        return Ok(None);
    };

    let prompt = format!(
        "The following is a timeline of an `omgb` run that used many tool calls, encountered errors and eventually succeeded, \
         or required explicit user corrections. Create a concise, reusable skill that would help avoid the errors, \
         apply the corrections, and complete the task faster.\n\n{}\n\n\
         Return a JSON object with fields: name, trigger, steps (list of strings), pitfalls (list of strings), verification (list of strings). \
         The trigger should be a short path or keyword substring that identifies when this skill applies (e.g. \"crates/oh-my-grok-build\" or \"rust\").",
        summarize_run(run)
    );

    let raw = llm_generate(&prompt).await?;
    let json = extract_json(&raw);
    let skill: Skill = serde_json::from_str(&json)
        .map_err(|e| anyhow::anyhow!("failed to parse generated skill: {e}\n{json}"))?;
    Ok(Some(skill))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_skill_markdown_roundtrip() {
        let skill = Skill {
            name: "Rust Refactor".into(),
            trigger: "crates/oh-my-grok-build".into(),
            steps: vec!["Run cargo clippy".into(), "Fix warnings".into()],
            pitfalls: vec!["Do not break tests".into()],
            verification: vec!["cargo test passes".into()],
            path: PathBuf::new(),
        };
        let md = format_skill_markdown(&skill).unwrap();
        let parsed = parse_skill_markdown(&md).unwrap();
        assert_eq!(parsed.name, skill.name);
        assert_eq!(parsed.trigger, skill.trigger);
        assert_eq!(parsed.steps, skill.steps);
        assert_eq!(parsed.pitfalls, skill.pitfalls);
        assert_eq!(parsed.verification, skill.verification);
    }

    #[test]
    fn test_trigger_matches() {
        assert!(trigger_matches("C:\\Users\\foo\\src", "src"));
        assert!(trigger_matches(
            "/home/foo/crates/oh-my-grok-build",
            "crates/oh-my-grok-build"
        ));
        assert!(!trigger_matches("/home/foo/bar", "baz"));
        assert!(trigger_matches("/any/cwd", ""));
    }

    #[test]
    fn test_extract_json() {
        let raw = "```json\n{\"name\":\"x\"}\n```";
        assert_eq!(extract_json(raw), "{\"name\":\"x\"}");
        let raw2 = "{\"name\":\"y\"}";
        assert_eq!(extract_json(raw2), "{\"name\":\"y\"}");
    }

    #[test]
    fn test_run_grouping_and_selection() {
        let t = Utc::now();
        let events = vec![
            TimelineEvent {
                timestamp: t,
                category: "exec".into(),
                message: "fix bug".into(),
                data: None,
            },
            TimelineEvent {
                timestamp: t,
                category: "tool".into(),
                message: "read".into(),
                data: Some(serde_json::json!({"tool_calls": 3})),
            },
            TimelineEvent {
                timestamp: t,
                category: "error".into(),
                message: "compile fail".into(),
                data: None,
            },
            TimelineEvent {
                timestamp: t,
                category: "tool".into(),
                message: "edit".into(),
                data: Some(serde_json::json!({"tool_calls": 2})),
            },
            TimelineEvent {
                timestamp: t,
                category: "success".into(),
                message: "done".into(),
                data: None,
            },
        ];
        let runs = group_runs(events);
        assert_eq!(runs.len(), 1);
        let tool_calls: usize = runs[0].iter().map(tool_call_count).sum();
        assert!(tool_calls >= 4);
        assert!(run_has_errors(&runs[0]));
        assert!(run_has_success(&runs[0]));
    }
}
