//! Multi-agent group chat for `omgb`.
//!
//! A group is a persisted chat room with 2–20 AI agents and any number of
//! human participants.  The host runs `omgb group chat <id>`; other humans
//! can post with `omgb group send <id> "<message>"` using the same group
//! file store.  Agents only reply when addressed, when the topic matches their
//! role, or when they have a relevant update, and `@mention` routing lets
//! agents ask each other directly without spawning reply loops.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;

use crate::args::{
    GroupApproveArgs, GroupArgs, GroupChatArgs, GroupCommand, GroupJoinArgs, GroupNewArgs,
    GroupSendArgs,
};

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
    #[serde(default)]
    pub host_name: String,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub pending_joins: Vec<JoinRequest>,
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug)]
pub(crate) enum GroupValidationError {
    Names(String),
    Model(String),
}

impl std::fmt::Display for GroupValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Names(s) | Self::Model(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for GroupValidationError {}

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

fn group_lock_path(id: &str) -> Result<PathBuf> {
    crate::threads::validate_id(id)?;
    Ok(groups_dir()?.join(format!("{id}.lock")))
}

fn modify_group<F, T>(id: &str, f: F) -> Result<T>
where
    F: FnOnce(&mut Group) -> Result<T>,
{
    let lock_path = group_lock_path(id)?;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;
    let mut group = load_group(id)?;
    let result = f(&mut group);
    if result.is_ok() {
        save_group(&group)?;
    }
    drop(lock_file);
    result
}

pub(crate) async fn modify_group_async<F, T>(id: &str, f: F) -> Result<T>
where
    F: FnOnce(&mut Group) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let id = id.to_string();
    tokio::task::spawn_blocking(move || modify_group(&id, f))
        .await
        .context("modify group task failed")?
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

pub(crate) fn load_group(id: &str) -> Result<Group> {
    let path = group_path(id)?;
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{e}"))
}

pub(crate) async fn load_group_async(id: &str) -> Result<Group> {
    let id = id.to_string();
    tokio::task::spawn_blocking(move || load_group(&id))
        .await
        .context("load group task failed")?
}

pub(crate) fn load_messages(id: &str) -> Result<Vec<GroupMessage>> {
    let path = messages_path(id)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut file = std::fs::OpenOptions::new().read(true).open(&path)?;
    file.lock_shared()?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .with_context(|| format!("read {}", path.display()))?;
    drop(file);
    raw.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("parse message line: {l}")))
        .collect()
}

pub(crate) async fn load_messages_async(id: &str) -> Result<Vec<GroupMessage>> {
    let id = id.to_string();
    tokio::task::spawn_blocking(move || load_messages(&id))
        .await
        .context("load messages task failed")?
}

pub(crate) fn add_message(id: &str, message: &GroupMessage) -> Result<()> {
    let path = messages_path(id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existed = path.exists();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&path)?;
    file.lock_exclusive()?;
    let line = serde_json::to_string(message)?;
    writeln!(file, "{line}")?;
    drop(file);
    if !existed {
        crate::providers::restrict_omg_file_permissions(&path)?;
    }
    Ok(())
}

pub(crate) async fn add_message_async(id: &str, message: &GroupMessage) -> Result<()> {
    let id = id.to_string();
    let message = message.clone();
    tokio::task::spawn_blocking(move || add_message(&id, &message))
        .await
        .context("add message task failed")?
}

pub async fn run_group(args: &GroupArgs) -> Result<()> {
    match &args.command {
        GroupCommand::New(args) => new_group(args).await,
        GroupCommand::List => list_groups(),
        GroupCommand::Show { id } => show_group(id),
        GroupCommand::Chat(args) => {
            let human_name = args.human_name.clone().unwrap_or_else(default_human_name);
            if let Some(remote) = args.remote.as_deref() {
                let token = args
                    .token
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("--token is required for remote group chat"))?;
                chat_remote(&args.id, token, &human_name, remote).await
            } else {
                chat(args).await
            }
        }
        GroupCommand::Send(args) => {
            if let Some(remote) = args.remote.as_deref() {
                let token = args
                    .token
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("--token is required for remote group send"))?;
                let sender = args.human_name.clone().unwrap_or_else(default_human_name);
                let message = GroupMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    timestamp: Utc::now(),
                    sender,
                    content: args.message.clone(),
                    kind: MessageKind::Human,
                };
                send_remote(&args.id, token, &message, remote).await?;
                println!("sent message to remote group {}", args.id);
                Ok(())
            } else {
                send(args).await
            }
        }
        GroupCommand::Join(args) => {
            if let Some(remote) = args.remote.as_deref() {
                let token = args
                    .token
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("--token is required for remote group join"))?;
                join_remote(&args.id, token, args, remote).await
            } else {
                join_local(&args.id, args).await
            }
        }
        GroupCommand::Approve(args) => {
            if let Some(remote) = args.remote.as_deref() {
                let token = args.token.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("--token is required for remote group approve")
                })?;
                approve_remote(&args.id, &args.request_id, token, args, remote).await
            } else {
                approve_local(&args.id, &args.request_id).await
            }
        }
        GroupCommand::Invite { id } => invite(id),
    }
}

pub(crate) struct GroupSpec {
    pub model: String,
    pub names: Vec<String>,
    pub roles: Vec<String>,
    pub agent_models: Vec<String>,
    pub host_name: String,
}

pub(crate) async fn validate_group_create(
    args: &GroupNewArgs,
) -> std::result::Result<GroupSpec, GroupValidationError> {
    let (count, names, roles) =
        parse_agent_specs(args).map_err(|e| GroupValidationError::Names(e.to_string()))?;

    let model = match &args.model {
        Some(m) => {
            let model = normalize_model(m);
            if model.is_empty() {
                return Err(GroupValidationError::Model(format!(
                    "invalid group model '{m}'"
                )));
            }
            let provider_id = model.strip_prefix("omgb-").unwrap_or(&model);
            if crate::providers::get_provider(provider_id)
                .ok()
                .flatten()
                .is_none()
                && crate::providers::provider_template(provider_id).is_none()
            {
                return Err(GroupValidationError::Model(format!(
                    "unknown group model '{model}'; pass a provider id (e.g. xai, openai) or known model name"
                )));
            }
            model
        }
        None => {
            let task = args.description.as_deref().unwrap_or(&args.name);
            let provider = crate::moe::select_provider_or_fallback(task)
                .await
                .map_err(|e| GroupValidationError::Model(e.to_string()))?;
            format!("omgb-{provider}")
        }
    };

    let agent_models = parse_agent_models(args, count, &model);
    validate_agent_models(&agent_models).map_err(|e| GroupValidationError::Model(e.to_string()))?;

    let host_name = args
        .human_name
        .clone()
        .unwrap_or_else(default_human_name)
        .trim()
        .to_string();
    validate_human_name(&host_name).map_err(|e| GroupValidationError::Names(e.to_string()))?;
    if names.iter().any(|n| n.eq_ignore_ascii_case(&host_name)) {
        return Err(GroupValidationError::Names(format!(
            "host name '{host_name}' conflicts with an agent name"
        )));
    }

    Ok(GroupSpec {
        model,
        names,
        roles,
        agent_models,
        host_name,
    })
}

pub(crate) fn build_group(args: &GroupNewArgs, spec: GroupSpec) -> Result<Group> {
    let mut agents = Vec::with_capacity(spec.names.len());
    for ((name, role), m) in spec
        .names
        .iter()
        .zip(spec.roles.iter())
        .zip(spec.agent_models.iter())
    {
        agents.push(Agent {
            id: slugify(name),
            name: name.clone(),
            role: role.clone(),
            model: m.clone(),
        });
    }

    let group = Group {
        id: uuid::Uuid::new_v4().to_string(),
        name: args.name.clone(),
        description: args.description.clone().unwrap_or_default(),
        created_at: Utc::now(),
        model: spec.model,
        yolo: args.yolo,
        invite_token: uuid::Uuid::new_v4().to_string().replace('-', ""),
        host_name: spec.host_name.clone(),
        members: vec![spec.host_name],
        pending_joins: Vec::new(),
        agents,
    };

    save_group(&group)?;
    Ok(group)
}

pub(crate) async fn create_group(args: &GroupNewArgs) -> Result<Group> {
    let spec = validate_group_create(args).await?;
    build_group(args, spec)
}

pub(crate) fn is_member(group: &Group, name: &str) -> bool {
    group.members.iter().any(|m| m.eq_ignore_ascii_case(name))
}

pub(crate) fn validate_human_name(name: &str) -> Result<()> {
    let n = name.trim();
    if n.is_empty() || n.eq_ignore_ascii_case("all") {
        bail!("invalid human name '{n}'; cannot be empty or 'all'");
    }
    if n.len() > 32 {
        bail!("human name '{n}' is too long (max 32 characters)");
    }
    if !n
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        bail!("human name '{n}' must contain only letters, digits, '-', or '_'");
    }
    Ok(())
}

pub(crate) fn validate_member_name(group: &Group, name: &str) -> Result<()> {
    validate_human_name(name)?;
    let n = name.trim();
    if group.agents.iter().any(|a| a.name.eq_ignore_ascii_case(n)) {
        bail!("human name '{n}' conflicts with an agent name");
    }
    Ok(())
}

pub(crate) fn ensure_local_member(group: &mut Group, name: &str) -> Result<()> {
    validate_member_name(group, name)?;
    if !is_member(group, name) {
        group.members.push(name.trim().to_string());
    }
    Ok(())
}

pub(crate) fn add_join_request(
    group: &mut Group,
    name: &str,
    github: Option<&str>,
) -> Result<String> {
    validate_member_name(group, name)?;
    let n = name.trim();
    if is_member(group, n) {
        bail!("'{n}' is already a member of this group");
    }
    if group
        .pending_joins
        .iter()
        .any(|r| r.name.eq_ignore_ascii_case(n))
    {
        bail!("a join request for '{n}' is already pending");
    }
    let id = uuid::Uuid::new_v4().to_string();
    group.pending_joins.push(JoinRequest {
        id: id.clone(),
        name: n.to_string(),
        github: github
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        requested_at: Utc::now(),
    });
    Ok(id)
}

pub(crate) fn approve_join_request(group: &mut Group, request_id: &str) -> Result<String> {
    let pos = group
        .pending_joins
        .iter()
        .position(|r| r.id == request_id)
        .ok_or_else(|| anyhow::anyhow!("join request {request_id} not found"))?;
    let req = group.pending_joins.remove(pos);
    if !is_member(group, &req.name) {
        group.members.push(req.name.clone());
    }
    Ok(req.name)
}

async fn new_group(args: &GroupNewArgs) -> Result<()> {
    let group = create_group(args).await?;

    println!("created group {}: {}", group.id, group.name);
    println!("  agents:");
    for a in &group.agents {
        println!("    {} ({}) — {}", a.name, a.model, a.role);
    }
    println!(
        "\nhost chat:    omgb group chat {} --token {}",
        group.id, group.invite_token
    );
    println!(
        "send message: omgb group send {} \"<message>\" --token {}",
        group.id, group.invite_token
    );
    println!(
        "invite link:  omgb://group/{}?token={}",
        group.id, group.invite_token
    );
    if let Ok(remote) = std::env::var("OMGB_REMOTE") {
        let remote = remote.trim_end_matches('/');
        println!(
            "http invite:  {remote}/group/{}?token={}",
            group.id, group.invite_token
        );
    }
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
    println!("  omgb group chat {id} --token {}", group.invite_token);
    println!(
        "  omgb group send {id} \"<message>\" --token {}",
        group.invite_token
    );
    println!("  omgb://group/{id}?token={}", group.invite_token);
    if let Ok(remote) = std::env::var("OMGB_REMOTE") {
        let remote = remote.trim_end_matches('/');
        println!("  {remote}/group/{id}?token={}", group.invite_token);
    }
    Ok(())
}

async fn send(args: &GroupSendArgs) -> Result<()> {
    let group = load_group_async(&args.id).await?;
    validate_token(&group, args.token.as_deref())?;
    let sender = args.human_name.clone().unwrap_or_else(default_human_name);
    let sender_for_modify = sender.clone();
    modify_group_async(&args.id, move |g| {
        ensure_local_member(g, &sender_for_modify)
    })
    .await?;
    let group = load_group_async(&args.id).await?;
    let message = GroupMessage {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        sender,
        content: args.message.clone(),
        kind: MessageKind::Human,
    };
    add_message_async(&group.id, &message).await?;
    let sender = message.sender.clone();
    let group_for_dispatch = group.clone();
    let runtime_handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let _ = runtime_handle.block_on(dispatch_for_message(group_for_dispatch, message, sender));
    });
    println!("sent message to group {} ({})", group.id, group.name);
    Ok(())
}

fn print_pending_alert(requests: &[JoinRequest]) {
    println!("\x1b[1;33m");
    println!("*** PENDING JOIN REQUESTS: {} ***", requests.len());
    for r in requests {
        let gh = r.github.as_deref().unwrap_or("-");
        println!("  {}: {} (github: {})", r.id, r.name, gh);
    }
    println!("Approve with: omgb group approve <id> <request_id>");
    println!("\x1b[0m");
}

async fn chat(args: &GroupChatArgs) -> Result<()> {
    let group = load_group_async(&args.id).await?;
    validate_token(&group, args.token.as_deref())?;
    let human_name = args.human_name.clone().unwrap_or_else(default_human_name);
    let modify_name = human_name.clone();
    modify_group_async(&args.id, move |g| ensure_local_member(g, &modify_name)).await?;
    let group = load_group_async(&args.id).await?;
    let yolo = args.yolo || group.yolo;

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
    println!("members: {}", group.members.join(", "));
    if !group.pending_joins.is_empty() {
        print_pending_alert(&group.pending_joins);
    }
    println!("type a message and press Enter. /quit or /exit to leave.\n");

    let initial = load_messages_async(&group.id).await?;
    let mut seen: HashSet<String> = initial.iter().map(|m| m.id.clone()).collect();
    for m in &initial {
        print_message(m);
    }

    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut input = String::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pending_count = group.pending_joins.len();

    loop {
        input.clear();
        tokio::select! {
            _ = interval.tick() => {
                let fresh = load_messages_async(&group.id).await?;
                let new_messages: Vec<GroupMessage> = fresh
                    .iter()
                    .filter(|m| !seen.contains(&m.id))
                    .cloned()
                    .collect();
                for m in &new_messages {
                    print_message(m);
                }

                if let Ok(fresh_group) = load_group_async(&group.id).await
                    && fresh_group.pending_joins.len() != last_pending_count
                {
                    last_pending_count = fresh_group.pending_joins.len();
                    print_pending_alert(&fresh_group.pending_joins);
                }

                for m in &new_messages {
                    if seen.contains(&m.id) {
                        continue;
                    }
                    seen.insert(m.id.clone());
                    if !matches!(m.kind, MessageKind::Agent) && m.sender != human_name {
                        dispatch_turn(&group, m, &human_name, yolo, &mut seen).await?;
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
                    sender: human_name.clone(),
                    content: text.into(),
                    kind: MessageKind::User,
                };
                add_message_async(&group.id, &message).await?;
                print_message(&message);
                seen.insert(message.id.clone());
                dispatch_turn(&group, &message, &human_name, yolo, &mut seen).await?;
            }
        }
    }

    println!("\nleft group {}", group.id);
    Ok(())
}

#[derive(serde::Deserialize)]
struct RemoteGroupInfo {
    name: String,
    #[serde(default)]
    description: String,
    model: String,
    #[serde(default)]
    yolo: bool,
    #[serde(default)]
    host_name: String,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    pending_joins: Vec<JoinRequest>,
    agents: Vec<Agent>,
}

pub(crate) async fn chat_remote(
    id: &str,
    token: &str,
    human_name: &str,
    remote: &str,
) -> Result<()> {
    let remote = remote.trim_end_matches('/');
    let info_url = format!("{remote}/group/{id}");
    let messages_url = format!("{remote}/group/{id}/messages");
    let joins_url = format!("{remote}/group/{id}/joins");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let group: Group = match client
        .get(&info_url)
        .header("x-group-token", token)
        .send()
        .await
    {
        Ok(res) if res.status().is_success() => {
            let info = res
                .json::<RemoteGroupInfo>()
                .await
                .map_err(|e| anyhow::anyhow!("failed to parse group info: {e}"))?;
            Group {
                id: id.to_string(),
                name: info.name,
                description: info.description,
                created_at: Utc::now(),
                model: info.model,
                yolo: info.yolo,
                invite_token: token.to_string(),
                host_name: info.host_name,
                members: info.members,
                pending_joins: info.pending_joins,
                agents: info.agents,
            }
        }
        Ok(res) => bail!("failed to fetch group info: {}", res.status()),
        Err(e) => bail!("failed to fetch group info: {e}"),
    };

    let can_send = is_member(&group, human_name);

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
    println!("members: {}", group.members.join(", "));
    if !can_send {
        println!("\x1b[1;31m*** You ('{human_name}') are not yet an approved member. ***\x1b[0m");
        println!(
            "Run: omgb group join {id} --token <token> --remote {remote} --name <your-name> [--github <github>]"
        );
    }
    if !group.pending_joins.is_empty() {
        print_pending_alert(&group.pending_joins);
    }
    println!("type a message and press Enter. /quit or /exit to leave.\n");

    let mut seen: HashSet<String> = HashSet::new();
    match client
        .get(&messages_url)
        .header("x-group-token", token)
        .send()
        .await
    {
        Ok(res) if res.status().is_success() => {
            if let Ok(initial) = res.json::<Vec<GroupMessage>>().await {
                for m in &initial {
                    print_message(m);
                    seen.insert(m.id.clone());
                }
            }
        }
        Ok(res) => eprintln!("warning: failed to fetch messages: {}", res.status()),
        Err(e) => eprintln!("warning: failed to fetch messages: {e}"),
    }

    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut input = String::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(2500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pending_count = group.pending_joins.len();

    loop {
        input.clear();
        tokio::select! {
            _ = interval.tick() => {
                match client.get(&messages_url).header("x-group-token", token).send().await {
                    Ok(res) if res.status().is_success() => {
                        if let Ok(fresh) = res.json::<Vec<GroupMessage>>().await {
                            for m in fresh {
                                if seen.insert(m.id.clone()) {
                                    print_message(&m);
                                }
                            }
                        }
                    }
                    Ok(res) => eprintln!("warning: failed to poll messages: {}", res.status()),
                    Err(e) => eprintln!("warning: failed to poll messages: {e}"),
                }

                match client.get(&joins_url).header("x-group-token", token).send().await {
                    Ok(res) if res.status().is_success() => {
                        if let Ok(fresh) = res.json::<Vec<JoinRequest>>().await
                            && fresh.len() != last_pending_count
                        {
                            last_pending_count = fresh.len();
                            print_pending_alert(&fresh);
                        }
                    }
                    Ok(res) => eprintln!("warning: failed to poll join requests: {}", res.status()),
                    Err(e) => eprintln!("warning: failed to poll join requests: {e}"),
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
                if !can_send {
                    eprintln!("\x1b[1;31m*** You are not an approved member. Run `omgb group join ...` first. ***\x1b[0m");
                    continue;
                }

                let message = GroupMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    timestamp: Utc::now(),
                    sender: human_name.to_string(),
                    content: text.to_string(),
                    kind: MessageKind::User,
                };
                if let Err(e) = send_remote(id, token, &message, remote).await {
                    eprintln!("warning: failed to send message: {e}");
                } else {
                    print_message(&message);
                    seen.insert(message.id.clone());
                }
            }
        }
    }

    println!("\nleft group {}", id);
    Ok(())
}

pub(crate) async fn send_remote(
    id: &str,
    token: &str,
    message: &GroupMessage,
    remote: &str,
) -> Result<()> {
    let remote = remote.trim_end_matches('/');
    let url = format!("{remote}/group/{id}/messages");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let res = client
        .post(&url)
        .header("x-group-token", token)
        .json(message)
        .send()
        .await?;
    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        bail!("failed to post message: {text}");
    }
    Ok(())
}

async fn read_line_prompt(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::Write::flush(&mut std::io::stdout())?;
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    Ok(line.trim().to_string())
}

async fn join_local(id: &str, args: &GroupJoinArgs) -> Result<()> {
    let name = if let Some(n) = args.name.as_deref() {
        n.to_string()
    } else {
        read_line_prompt("Enter your name: ").await?
    };
    let github = args.github.clone();
    let modify_name = name.clone();
    let (status, request_id) = modify_group_async(id, move |group| {
        if group.members.is_empty() {
            // Legacy group: first human to join is auto-approved.
            ensure_local_member(group, &modify_name)?;
            return Ok(("approved".to_string(), String::new()));
        }
        if is_member(group, &modify_name) {
            return Ok(("approved".to_string(), String::new()));
        }
        let request_id = add_join_request(group, &modify_name, github.as_deref())?;
        Ok(("pending".to_string(), request_id))
    })
    .await?;
    if status == "approved" {
        println!("'{name}' joined group {id} (auto-approved as first member)");
    } else {
        println!("join request {request_id} for '{name}' is pending approval in group {id}");
        println!("an existing member can approve with: omgb group approve {id} {request_id}");
    }
    Ok(())
}

async fn approve_local(id: &str, request_id: &str) -> Result<()> {
    let request_id = request_id.to_string();
    let modify_request_id = request_id.clone();
    let name = modify_group_async(id, move |group| {
        approve_join_request(group, &modify_request_id)
    })
    .await?;
    println!("approved join request {request_id}: '{name}' can now post in group {id}");
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JoinResult {
    pub id: String,
    pub status: String,
    pub name: String,
    pub github: Option<String>,
}

async fn join_remote(id: &str, token: &str, args: &GroupJoinArgs, remote: &str) -> Result<()> {
    let remote = remote.trim_end_matches('/');
    let url = format!("{remote}/group/{id}/join");
    let name = if let Some(n) = args.name.as_deref() {
        n.to_string()
    } else {
        read_line_prompt("Enter your name: ").await?
    };
    let github = args.github.as_deref();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let body = serde_json::json!({
        "name": name,
        "github": github,
    });
    let res = client
        .post(&url)
        .header("x-group-token", token)
        .json(&body)
        .send()
        .await?;
    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        bail!("failed to request join: {text}");
    }
    let result: JoinResult = res.json().await?;
    if result.status == "approved" {
        println!("'{name}' was auto-approved for group {id}");
    } else {
        println!(
            "join request {} for '{name}' is pending approval in group {id}",
            result.id
        );
        println!(
            "an existing member can approve with: omgb group approve {id} {} --token <token> --remote {remote}",
            result.id
        );
    }
    Ok(())
}

async fn approve_remote(
    id: &str,
    request_id: &str,
    token: &str,
    args: &GroupApproveArgs,
    remote: &str,
) -> Result<()> {
    let remote = remote.trim_end_matches('/');
    let url = format!("{remote}/group/{id}/joins/{request_id}/approve");
    let approver = args.name.clone().unwrap_or_else(default_human_name);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let res = client
        .post(&url)
        .header("x-group-token", token)
        .header("x-approver-name", approver)
        .send()
        .await?;
    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        bail!("failed to approve join: {text}");
    }
    let result: JoinResult = res.json().await?;
    println!(
        "approved join request {request_id}: '{}' can now post in group {id}",
        result.name
    );
    Ok(())
}

async fn dispatch_turn(
    group: &Group,
    trigger: &GroupMessage,
    human_name: &str,
    yolo: bool,
    seen: &mut HashSet<String>,
) -> Result<()> {
    let lock = tokio::task::spawn_blocking({
        let id = group.id.clone();
        move || -> Result<Option<std::fs::File>> {
            let path = dispatch_lock_path(&id)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&path)?;
            match file.try_lock_exclusive() {
                Ok(()) => Ok(Some(file)),
                Err(e) => {
                    if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                        Ok(None)
                    } else {
                        Err(e.into())
                    }
                }
            }
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("dispatch lock join failed: {e}"))??;

    let Some(_lock) = lock else {
        return Ok(());
    };

    let mut messages = load_messages_async(&group.id).await?;
    let Some(idx) = messages.iter().position(|m| m.id == trigger.id) else {
        return Ok(());
    };
    if messages
        .iter()
        .skip(idx + 1)
        .any(|m| matches!(m.kind, MessageKind::Agent))
    {
        seen.insert(trigger.id.clone());
        return Ok(());
    }

    seen.insert(trigger.id.clone());

    let prompt = build_routing_prompt(group, &messages[..=idx], trigger, human_name);
    let reply = match crate::run_single_turn_capture(
        &prompt,
        Some(group.model.clone()),
        yolo,
        Some(1),
        None,
    )
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
        add_message_async(&group.id, &message).await?;
        messages.push(message.clone());
        print_message(&message);
        seen.insert(message.id.clone());
    }

    // One round of @mention routing so agents can ask each other directly.
    // Only consider messages not already handled in a previous dispatch so
    // the same mention pair does not trigger repeatedly.
    let mut mention_targets: Vec<(String, String, String)> = Vec::new();
    for m in messages.iter().rev().take(HISTORY_LIMIT) {
        if matches!(m.kind, MessageKind::Agent) && !seen.contains(&m.id) {
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
                format_history(&messages, HISTORY_LIMIT),
                sender,
                context
            );
            let model = agent_model(group, &agent.name);
            let content =
                match crate::run_single_turn_capture(&prompt, Some(model), yolo, Some(1), None)
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
            add_message_async(&group.id, &message).await?;
            messages.push(message.clone());
            print_message(&message);
            seen.insert(message.id.clone());
        }
    }

    Ok(())
}

/// Dispatch agent replies for a single message, seeding `seen` with existing
/// messages so old @mentions are not re-processed.
pub(crate) async fn dispatch_for_message(
    group: Group,
    trigger: GroupMessage,
    human_name: String,
) -> Result<()> {
    let yolo = group.yolo;
    let mut seen = HashSet::new();
    for m in load_messages_async(&group.id).await? {
        seen.insert(m.id);
    }
    dispatch_turn(&group, &trigger, &human_name, yolo, &mut seen).await
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
    if messages.last().is_none_or(|m| m.id != trigger.id) {
        prompt.push_str(&format!(
            "\n[{}] {}: {}\n",
            trigger.timestamp.format("%Y-%m-%d %H:%M UTC"),
            trigger.sender,
            trigger.content
        ));
    }
    prompt.push_str("\nJSON replies:\n");
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
    let value = match crate::extract_json_object(text) {
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

fn parse_mentions(text: &str, agents: &[Agent]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (i, _) in text.match_indices('@') {
        if let Some(prev) = text[..i].chars().next_back()
            && !prev.is_whitespace()
            && !matches!(prev, '(' | '[' | '{' | '"' | '\'' | '<' | '>' | '`')
        {
            continue;
        }
        let rest = &text[i + 1..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
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
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let roles: Vec<String> = args
        .roles
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let count = if let Some(n) = args.count {
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

    // Validate unique, mention-friendly names.
    let mut seen = HashSet::new();
    for name in &final_names {
        if name.len() > 32 {
            bail!("agent name '{name}' is too long (max 32 characters)");
        }
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            bail!("agent name '{name}' must contain only letters, digits, '-', or '_'");
        }
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

pub(crate) fn normalize_model(model: &str) -> String {
    let m = model.trim();
    if m.is_empty() {
        return String::new();
    }
    if let Some(rest) = m.strip_prefix("omgb-") {
        if let Some(id) = crate::providers::resolve_model_to_provider(rest) {
            return format!("omgb-{id}");
        }
        return m.to_string();
    }
    if let Some(id) = crate::providers::resolve_model_to_provider(m) {
        return format!("omgb-{id}");
    }
    m.to_string()
}

fn default_human_name() -> String {
    std::env::var("USER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("USERNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "human".to_string())
}

fn validate_token(group: &Group, token: Option<&str>) -> Result<()> {
    let t = token.unwrap_or("");
    if t.is_empty() || t != group.invite_token {
        bail!(
            "group {} requires a valid invite token; use --token <token>",
            group.id
        );
    }
    Ok(())
}

fn dispatch_lock_path(id: &str) -> Result<PathBuf> {
    crate::threads::validate_id(id)?;
    Ok(groups_dir()?.join(format!("{id}.dispatch.lock")))
}

fn agent_model(group: &Group, name: &str) -> String {
    group
        .agents
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(name))
        .and_then(|a| {
            let m = a.model.trim();
            if m.is_empty() {
                None
            } else {
                Some(m.to_string())
            }
        })
        .unwrap_or_else(|| group.model.clone())
}

fn parse_agent_models(args: &GroupNewArgs, count: usize, fallback: &str) -> Vec<String> {
    let mut models: Vec<String> = args
        .models
        .iter()
        .map(|s| normalize_model(s.trim()))
        .filter(|s| !s.is_empty())
        .collect();
    models.resize(count, fallback.to_string());
    models
}

fn validate_agent_models(models: &[String]) -> Result<()> {
    for m in models {
        let provider_id = m.strip_prefix("omgb-").unwrap_or(m);
        if crate::providers::get_provider(provider_id)
            .ok()
            .flatten()
            .is_none()
            && crate::providers::provider_template(provider_id).is_none()
        {
            bail!("unknown per-agent model '{m}'; pass a provider id or known model name");
        }
    }
    Ok(())
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

        let m = parse_mentions("email me at alice@example.com or use array@index", &agents);
        assert!(m.is_empty());
    }

    #[test]
    fn extract_json_object_finds_object() {
        let text = "Some text before {\"replies\":{\"Alice\":\"ok\"}} after";
        let value = crate::extract_json_object(text).unwrap();
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
            names: vec!["alice".into(), "bob".into()],
            roles: vec!["coder".into(), "reviewer".into()],
            models: vec![],
            human_name: None,
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
        assert_eq!(normalize_model("grok-3"), "omgb-xai");
        assert_eq!(normalize_model("omgb-grok-3"), "omgb-xai");
        assert_eq!(
            normalize_model("not-a-real-provider"),
            "not-a-real-provider"
        );
    }
}
