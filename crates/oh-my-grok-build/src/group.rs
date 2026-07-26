//! Multi-agent group chat for `omgb`.
//!
//! A group is a persisted chat room with 2–20 AI agents and any number of
//! human participants.  The host runs `omgb group chat <id>`; other humans
//! can post with `omgb group send <id> "<message>"` using the same group
//! file store.  Agents only reply when addressed, when the topic matches their
//! role, or when they have a relevant update, and `@mention` routing lets
//! agents ask each other directly without spawning reply loops.

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;

use crate::args::{GroupArgs, GroupChatArgs, GroupCommand, GroupNewArgs, GroupSendArgs};

const MAX_AGENTS: usize = 20;
const MIN_AGENTS: usize = 2;
const HISTORY_LIMIT: usize = 50;
const MENTION_LIMIT: usize = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub model: String,
    pub yolo: bool,
    pub invite_token: String,
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMessage {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub sender: String,
    pub content: String,
    pub kind: MessageKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    User,
    Human,
    Agent,
}

fn groups_dir() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("groups"))
}

fn group_path(id: &str) -> Result<PathBuf> {
    crate::threads::validate_id(id)?;
    Ok(groups_dir()?.join(format!("{id}.json")))
}

fn messages_path(id: &str) -> Result<PathBuf> {
    crate::threads::validate_id(id)?;
    Ok(groups_dir()?.join(format!("{id}.messages.jsonl")))
}

fn save_group(group: &Group) -> Result<()> {
    let path = group_path(&group.id)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("groups path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    crate::providers::write_file_atomic(&path, serde_json::to_string_pretty(group)?, true)
        .with_context(|| format!("write {}", path.display()))
}

fn load_group(id: &str) -> Result<Group> {
    let path = group_path(id)?;
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{e}"))
}

fn load_messages(id: &str) -> Result<Vec<GroupMessage>> {
    let path = messages_path(id)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::OpenOptions::new().read(true).open(&path)?;
    file.lock_shared()?;
    let raw = std::fs::read_to_string(&path)?;
    drop(file);
    raw.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("parse message line: {l}")))
        .collect()
}

fn add_message(id: &str, message: &GroupMessage) -> Result<()> {
    let path = messages_path(id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&path)?;
    file.lock_exclusive()?;
    let line = serde_json::to_string(message)?;
    writeln!(file, "{line}")?;
    drop(file);
    Ok(())
}

pub async fn run_group(args: &GroupArgs) -> Result<()> {
    match &args.command {
        GroupCommand::New(args) => new_group(args).await,
        GroupCommand::List => list_groups(),
        GroupCommand::Show { id } => show_group(id),
        GroupCommand::Chat(args) => chat(args).await,
        GroupCommand::Send(args) => send(args).await,
        GroupCommand::Invite { id } => invite(id),
    }
}

async fn new_group(args: &GroupNewArgs) -> Result<()> {
    let (_count, names, roles) = parse_agent_specs(args)?;

    let model = match &args.model {
        Some(m) => normalize_model(m),
        None => {
            let task = args.description.as_deref().unwrap_or(&args.name);
            let provider = crate::moe::select_provider_or_fallback(task).await?;
            format!("omgb-{provider}")
        }
    };

    let mut agents = Vec::with_capacity(names.len());
    for (name, role) in names.iter().zip(roles.iter()) {
        agents.push(Agent {
            id: slugify(name),
            name: name.clone(),
            role: role.clone(),
            model: model.clone(),
        });
    }

    let group = Group {
        id: uuid::Uuid::new_v4().to_string(),
        name: args.name.clone(),
        description: args.description.clone().unwrap_or_default(),
        created_at: Utc::now(),
        model,
        yolo: args.yolo,
        invite_token: uuid::Uuid::new_v4().to_string().replace('-', ""),
        agents,
    };

    save_group(&group)?;

    println!("created group {}: {}", group.id, group.name);
    println!("  agents:");
    for a in &group.agents {
        println!("    {} ({}) — {}", a.name, a.model, a.role);
    }
    println!("\nhost chat:    omgb group chat {}", group.id);
    println!("send message: omgb group send {} \"<message>\"", group.id);
    println!(
        "invite link:  omgb://group/{}?token={}",
        group.id, group.invite_token
    );
    Ok(())
}

fn list_groups() -> Result<()> {
    let dir = groups_dir()?;
    if !dir.exists() {
        println!("no groups");
        return Ok(());
    }
    let mut groups = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json")
            && path.file_name().is_some_and(|n| n != ".messages.jsonl")
            && let Ok(raw) = std::fs::read_to_string(&path)
            && let Ok(group) = serde_json::from_str::<Group>(&raw)
        {
            groups.push(group);
        }
    }
    if groups.is_empty() {
        println!("no groups");
        return Ok(());
    }
    groups.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    for g in groups {
        println!("{}  {}  ({} agents)", g.id, g.name, g.agents.len());
    }
    Ok(())
}

fn show_group(id: &str) -> Result<()> {
    let group = load_group(id)?;
    println!("group {}: {}", group.id, group.name);
    if !group.description.is_empty() {
        println!("description: {}", group.description);
    }
    println!("model: {}", group.model);
    println!("agents:");
    for a in &group.agents {
        println!("  {} ({}) — {}", a.name, a.model, a.role);
    }
    let messages = load_messages(id)?;
    if !messages.is_empty() {
        println!("\nmessages (last {} shown):", messages.len().min(30));
        let start = messages.len().saturating_sub(30);
        for m in &messages[start..] {
            print_message(m);
        }
    }
    Ok(())
}

fn invite(id: &str) -> Result<()> {
    let group = load_group(id)?;
    println!(
        "share this with humans/agents to join group {}:\n",
        group.name
    );
    println!("  omgb group chat {id}");
    println!("  omgb group send {id} \"<message>\"");
    println!("  omgb://group/{id}?token={}", group.invite_token);
    Ok(())
}

async fn send(args: &GroupSendArgs) -> Result<()> {
    let group = load_group(&args.id)?;
    let sender = args.human_name.as_deref().unwrap_or("human");
    let message = GroupMessage {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        sender: sender.into(),
        content: args.message.clone(),
        kind: MessageKind::Human,
    };
    add_message(&group.id, &message)?;
    println!("sent message to group {} ({})", group.id, group.name);
    Ok(())
}

async fn chat(args: &GroupChatArgs) -> Result<()> {
    let group = load_group(&args.id)?;
    let human_name = args.human_name.as_deref().unwrap_or("you");

    println!("group: {} ({})", group.name, group.id);
    println!(
        "agents: {}",
        group
            .agents
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("type a message and press Enter. /quit or /exit to leave.\n");

    let mut messages = load_messages(&group.id)?;
    let mut processed: HashSet<String> = messages.iter().map(|m| m.id.clone()).collect();
    for m in &messages {
        print_message(m);
    }

    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut input = String::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        input.clear();
        tokio::select! {
            _ = interval.tick() => {
                let fresh = load_messages(&group.id)?;
                if fresh.len() > messages.len() {
                    let old_len = messages.len();
                    messages = fresh;
                    let new_messages: Vec<GroupMessage> = messages[old_len..].to_vec();
                    for m in &new_messages {
                        print_message(m);
                    }
                    for m in &new_messages {
                        if !processed.contains(&m.id) && !matches!(m.kind, MessageKind::Agent) {
                            if m.sender != human_name {
                                dispatch_turn(&group, &mut messages, m, human_name).await?;
                            }
                            processed.insert(m.id.clone());
                        }
                    }
                }
            }
            res = reader.read_line(&mut input) => {
                if res.is_err() || res? == 0 {
                    break;
                }
                let text = input.trim();
                if text.is_empty() {
                    continue;
                }
                if text == "/quit" || text == "/exit" {
                    break;
                }

                let message = GroupMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    timestamp: Utc::now(),
                    sender: human_name.into(),
                    content: text.into(),
                    kind: MessageKind::User,
                };
                add_message(&group.id, &message)?;
                messages.push(message.clone());
                print_message(&message);
                dispatch_turn(&group, &mut messages, &message, human_name).await?;
                processed.insert(message.id);
            }
        }
    }

    println!("\nleft group {}", group.id);
    Ok(())
}

async fn dispatch_turn(
    group: &Group,
    messages: &mut Vec<GroupMessage>,
    trigger: &GroupMessage,
    human_name: &str,
) -> Result<()> {
    let prompt = build_routing_prompt(group, messages, trigger, human_name);
    let reply = match crate::run_single_turn_capture(&prompt, Some(group.model.clone()), group.yolo)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warning: group routing failed: {e}");
            return Ok(());
        }
    };

    let replies = parse_replies(&reply, &group.agents);
    for (name, content) in replies {
        let message = GroupMessage {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            sender: name,
            content,
            kind: MessageKind::Agent,
        };
        add_message(&group.id, &message)?;
        messages.push(message.clone());
        print_message(&message);
    }

    // One round of @mention routing so agents can ask each other directly.
    let mut mention_targets: Vec<(String, String, String)> = Vec::new();
    for m in messages.iter().rev().take(HISTORY_LIMIT) {
        if matches!(m.kind, MessageKind::Agent) {
            for target in parse_mentions(&m.content, &group.agents) {
                if target.eq_ignore_ascii_case(&m.sender) {
                    continue;
                }
                let key = format!("{}:{}", m.sender, target);
                if !mention_targets
                    .iter()
                    .any(|(s, t, _)| format!("{s}:{t}") == key)
                {
                    mention_targets.push((m.sender.clone(), target, m.content.clone()));
                }
            }
        }
    }

    for (sender, target_name, context) in mention_targets.into_iter().take(MENTION_LIMIT * 5) {
        if let Some(agent) = group
            .agents
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(&target_name))
        {
            let prompt = format!(
                "You are {} ({}) in the group chat.\n\n\
                 Conversation so far:\n{}\n\n\
                 {} mentioned you:\n{}\n\n\
                 Reply concisely. Do not ask follow-up questions unless essential.",
                agent.name,
                agent.role,
                format_history(messages, HISTORY_LIMIT),
                sender,
                context
            );
            let content = match crate::run_single_turn_capture(
                &prompt,
                Some(group.model.clone()),
                group.yolo,
            )
            .await
            {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("warning: mention reply failed: {e}");
                    continue;
                }
            };
            let trimmed = content.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("NO_REPLY") {
                continue;
            }
            let message = GroupMessage {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                sender: agent.name.clone(),
                content: trimmed.into(),
                kind: MessageKind::Agent,
            };
            add_message(&group.id, &message)?;
            messages.push(message.clone());
            print_message(&message);
        }
    }

    Ok(())
}

fn build_routing_prompt(
    group: &Group,
    messages: &[GroupMessage],
    trigger: &GroupMessage,
    _human_name: &str,
) -> String {
    let mut prompt = format!(
        "You are moderating a multi-agent group chat. The group is named \"{}\".\n\nAgents:\n",
        group.name
    );
    for a in &group.agents {
        prompt.push_str(&format!(
            "- {} (role: {}, model: {})\n",
            a.name, a.role, a.model
        ));
    }

    prompt.push_str("\nRules:\n");
    prompt.push_str("- An agent should reply ONLY if it is directly addressed (e.g. @name), if the topic strongly matches its role, or if it has a relevant update/status to share.\n");
    prompt.push_str("- Replies should be concise, in first person, and in character.\n");
    prompt.push_str("- Do not include an agent in the output if it has nothing to add.\n");
    prompt.push_str("- Humans outside the agent list should never appear in the replies.\n");
    prompt.push_str("- Output strictly valid JSON with this shape: {\"replies\":{\"AgentName\":\"reply text\",...}}.\n");
    prompt.push_str("- If no agent should reply, return {\"replies\":{}}.\n\n");

    prompt.push_str("Conversation history:\n");
    prompt.push_str(&format_history(messages, HISTORY_LIMIT));
    prompt.push_str(&format!(
        "\n[{}] {}: {}\n\nJSON replies:\n",
        trigger.timestamp.format("%Y-%m-%d %H:%M UTC"),
        trigger.sender,
        trigger.content
    ));
    prompt
}

fn format_history(messages: &[GroupMessage], limit: usize) -> String {
    let start = messages.len().saturating_sub(limit);
    messages[start..]
        .iter()
        .map(|m| {
            format!(
                "[{}] {}: {}",
                m.timestamp.format("%Y-%m-%d %H:%M UTC"),
                m.sender,
                m.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_replies(text: &str, agents: &[Agent]) -> Vec<(String, String)> {
    let value = match extract_json_object(text) {
        Some(v) => v,
        None => {
            eprintln!("warning: group routing did not return valid JSON: {text}");
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    if let Some(replies) = value.get("replies").and_then(|v| v.as_object()) {
        for (name, v) in replies {
            let content = v.as_str().unwrap_or("").trim();
            if content.is_empty() || content.eq_ignore_ascii_case("NO_REPLY") {
                continue;
            }
            if let Some(agent) = agents.iter().find(|a| a.name.eq_ignore_ascii_case(name)) {
                out.push((agent.name.clone(), content.to_string()));
            }
        }
    }
    out
}

fn extract_json_object(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let end = text.rfind('}').map(|i| i + 1)?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&text[start..end]).ok()
}

fn parse_mentions(text: &str, agents: &[Agent]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (i, _) in text.match_indices('@') {
        let rest = &text[i + 1..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
            .unwrap_or(rest.len());
        let name = &rest[..end];
        if name.is_empty() {
            continue;
        }
        let lower = name.to_lowercase();
        if lower == "all" {
            for a in agents {
                if seen.insert(a.name.to_lowercase()) {
                    out.push(a.name.clone());
                }
            }
        } else if let Some(agent) = agents.iter().find(|a| a.name.eq_ignore_ascii_case(name))
            && seen.insert(agent.name.to_lowercase())
        {
            out.push(agent.name.clone());
        }
    }
    out
}

fn print_message(m: &GroupMessage) {
    match m.kind {
        MessageKind::User | MessageKind::Human => println!(
            "[{}] {}: {}",
            m.timestamp.format("%H:%M"),
            m.sender,
            m.content
        ),
        MessageKind::Agent => println!(
            "[{}] {}: {}",
            m.timestamp.format("%H:%M"),
            m.sender,
            m.content
        ),
    }
}

fn parse_agent_specs(args: &GroupNewArgs) -> Result<(usize, Vec<String>, Vec<String>)> {
    let names: Vec<String> = args
        .names
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let roles: Vec<String> = args
        .roles
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let count = if let Some(n) = args.count {
        if n > names.len() && !names.is_empty() {
            bail!(
                "--count {n} is larger than the number of --names provided ({})",
                names.len()
            );
        }
        n.max(names.len()).max(MIN_AGENTS)
    } else if names.is_empty() {
        3
    } else {
        names.len()
    };

    if count < MIN_AGENTS {
        bail!("a group must have at least {MIN_AGENTS} agents");
    }
    if count > MAX_AGENTS {
        bail!("a group can have at most {MAX_AGENTS} agents");
    }

    let final_names: Vec<String> = names
        .iter()
        .cloned()
        .chain(default_agent_names().into_iter().skip(names.len()))
        .chain((1..=count).map(|i| format!("agent{i}")))
        .take(count)
        .collect();

    let final_roles: Vec<String> = roles
        .iter()
        .cloned()
        .chain(std::iter::repeat("generalist".to_string()))
        .take(count)
        .collect();

    // Validate unique names.
    let mut seen = HashSet::new();
    for name in &final_names {
        let lower = name.to_lowercase();
        if !seen.insert(lower.clone()) {
            bail!("agent names must be unique; duplicate: {name}");
        }
        if name.eq_ignore_ascii_case("all") {
            bail!("'all' is a reserved mention keyword and cannot be an agent name");
        }
    }

    Ok((count, final_names, final_roles))
}

fn default_agent_names() -> Vec<String> {
    vec![
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
        "lambda", "mu", "nu", "xi", "omicron", "pi", "rho", "sigma", "tau", "upsilon",
    ]
    .into_iter()
    .map(|s| s.to_string())
    .collect()
}

fn normalize_model(model: &str) -> String {
    if model.contains('-') && model.starts_with("omgb-") {
        model.to_string()
    } else if model.contains('-')
        || model
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        format!("omgb-{model}")
    } else {
        model.to_string()
    }
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
        .replace("--", "-")
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mentions_extracts_names_and_all() {
        let agents = vec![
            Agent {
                id: "alice".into(),
                name: "Alice".into(),
                role: "coder".into(),
                model: "omgb-openai".into(),
            },
            Agent {
                id: "bob".into(),
                name: "bob".into(),
                role: "reviewer".into(),
                model: "omgb-openai".into(),
            },
        ];
        let m = parse_mentions("@alice can you check this? @ALL @unknown", &agents);
        assert_eq!(m, vec!["Alice".to_string(), "bob".to_string()]);
    }

    #[test]
    fn extract_json_object_finds_object() {
        let text = "Some text before {\"replies\":{\"Alice\":\"ok\"}} after";
        let value = extract_json_object(text).unwrap();
        assert!(value.get("replies").is_some());
    }

    #[test]
    fn parse_replies_filters_no_reply() {
        let agents = vec![Agent {
            id: "a".into(),
            name: "Alpha".into(),
            role: "dev".into(),
            model: "omgb-openai".into(),
        }];
        let text = r#"{"replies":{"Alpha":"working on it","Beta":"NO_REPLY"}}"#;
        let replies = parse_replies(text, &agents);
        assert_eq!(replies, vec![("Alpha".into(), "working on it".into())]);
    }

    #[test]
    fn parse_agent_specs_uses_defaults_and_validates_count() {
        let args = GroupNewArgs {
            name: "test".into(),
            description: None,
            count: Some(2),
            model: None,
            names: Some("alice,bob".into()),
            roles: Some("coder,reviewer".into()),
            yolo: false,
        };
        let (count, names, roles) = parse_agent_specs(&args).unwrap();
        assert_eq!(count, 2);
        assert_eq!(names, vec!["alice", "bob"]);
        assert_eq!(roles, vec!["coder", "reviewer"]);
    }

    #[test]
    fn normalize_model_adds_omgb_prefix() {
        assert_eq!(normalize_model("openai"), "omgb-openai");
        assert_eq!(normalize_model("omgb-anthropic"), "omgb-anthropic");
    }
}
