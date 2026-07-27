//! Multi-agent group chat for `omgb`.
//!
//! A group is a persisted chat room with 2–20 AI agents and any number of
//! human participants.  The host runs `omgb group chat <id>`; other humans
//! can post with `omgb group send <id> "<message>"` using the same group
//! file store.  Agents only reply when addressed, when the topic matches their
//! role, or when they have a relevant update, and `@mention` routing lets
//! agents ask each other directly without spawning reply loops.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, Seek, SeekFrom, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;

use crate::args::{
    GroupApproveArgs, GroupArgs, GroupCommand, GroupHostAgentArgs, GroupJoinArgs, GroupNewArgs,
    GroupRemoteAgentAddArgs,
};

const MAX_AGENTS: usize = 20;
const MIN_AGENTS: usize = 2;
const HISTORY_LIMIT: usize = 50;
const MENTION_LIMIT: usize = 1;
const MAX_LOADED_MESSAGES: usize = 1000;
const MAX_GROUP_MESSAGES_BYTES: u64 = 10 * 1024 * 1024;
pub(crate) const MAX_GROUP_MESSAGE_BYTES: usize = 4096;

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
    pub member_tokens: HashMap<String, String>,
    #[serde(default, skip)]
    pub member_token_index: HashMap<String, String>,
    #[serde(default)]
    pub pending_joins: Vec<JoinRequest>,
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub remote_agents: Vec<RemoteAgent>,
    #[serde(default)]
    pub approved_member_tokens: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteAgent {
    pub name: String,
    pub role: String,
    pub model: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
    #[serde(default)]
    pub allow_local: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_heartbeat: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
    pub requested_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_auth_token: Option<String>,
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

fn membership_store_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("group_memberships.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MembershipStore {
    #[serde(default)]
    memberships: HashMap<String, Membership>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Membership {
    name: String,
    token: String,
}

fn membership_key(group_id: &str, name: &str) -> String {
    format!("{}:{}", group_id, name.trim().to_ascii_lowercase())
}

fn load_membership_store() -> Result<MembershipStore> {
    let path = membership_store_path()?;
    if !path.exists() {
        return Ok(MembershipStore::default());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut store: MembershipStore =
        serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut migrated = false;
    let old_keys: Vec<String> = store
        .memberships
        .keys()
        .filter(|k| !k.contains(':'))
        .cloned()
        .collect();
    for k in old_keys {
        if let Some(m) = store.memberships.remove(&k) {
            store.memberships.insert(membership_key(&k, &m.name), m);
            migrated = true;
        }
    }
    if migrated {
        save_membership_store(&store)?;
    }
    Ok(store)
}

fn save_membership_store(store: &MembershipStore) -> Result<()> {
    let path = membership_store_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::providers::write_file_atomic(&path, serde_json::to_string_pretty(store)?, true)
        .with_context(|| format!("write {}", path.display()))
}

pub(crate) fn load_membership(group_id: &str, name: &str) -> Option<Membership> {
    load_membership_store()
        .ok()?
        .memberships
        .get(&membership_key(group_id, name))
        .cloned()
}

pub(crate) fn save_membership(group_id: &str, name: &str, token: &str) -> Result<()> {
    let mut store = load_membership_store()?;
    store.memberships.insert(
        membership_key(group_id, name),
        Membership {
            name: name.trim().to_string(),
            token: token.to_string(),
        },
    );
    save_membership_store(&store)
}

pub(crate) fn load_membership_by_name(group_id: &str, name: &str) -> Option<Membership> {
    load_membership(group_id, name.trim())
}

pub(crate) fn generate_member_token() -> String {
    uuid::Uuid::new_v4().to_string().replace('-', "")
}

pub(crate) fn constant_time_token_eq(a: &str, b: &str) -> bool {
    constant_time_eq::constant_time_eq(a.as_bytes(), b.as_bytes())
}

fn recompute_member_token_index(group: &mut Group) {
    group.member_token_index.clear();
    for (name, token) in &group.member_tokens {
        group.member_token_index.insert(token.clone(), name.clone());
    }
}

pub(crate) fn validate_member_token(group: &Group, token: &str) -> Option<String> {
    let mut matched = None;
    for (stored, name) in &group.member_token_index {
        if constant_time_token_eq(stored, token) {
            matched = Some(name.clone());
        }
    }
    matched
}

pub(crate) fn issue_member_token(group: &mut Group, name: &str) -> Result<String> {
    let name = name.trim();
    validate_member_name(group, name)?;
    let canonical = group
        .members
        .iter()
        .find(|m| m.eq_ignore_ascii_case(name))
        .cloned()
        .unwrap_or_else(|| name.to_string());
    if !group
        .members
        .iter()
        .any(|m| m.eq_ignore_ascii_case(&canonical))
    {
        group.members.push(canonical.clone());
    }
    let token = group
        .member_tokens
        .entry(canonical.clone())
        .or_insert_with(generate_member_token)
        .clone();
    group.member_token_index.insert(token.clone(), canonical);
    Ok(token)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RemoteAgentDispatchPayload {
    pub group_id: String,
    pub agent_name: String,
    pub role: String,
    pub model: String,
    pub group_model: String,
    #[serde(default)]
    pub yolo: bool,
    pub prompt: String,
    pub history: Vec<GroupMessage>,
    pub message: GroupMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RemoteAgentDispatchResponse {
    pub content: String,
}

fn hosted_agents_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("hosted_agents.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HostedAgents {
    #[serde(default)]
    agents: HashMap<String, HostedAgent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostedAgent {
    group_id: String,
    name: String,
    token: String,
}

fn load_hosted_agents() -> Result<HostedAgents> {
    let path = hosted_agents_path()?;
    if !path.exists() {
        return Ok(HostedAgents::default());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{e}"))
}

fn save_hosted_agents(store: &HostedAgents) -> Result<()> {
    let path = hosted_agents_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::providers::write_file_atomic(&path, serde_json::to_string_pretty(store)?, true)
        .with_context(|| format!("write {}", path.display()))
}

pub(crate) fn register_hosted_agent(group_id: &str, name: &str, token: &str) -> Result<()> {
    let mut store = load_hosted_agents()?;
    let key = format!("{}:{}", group_id, name.trim().to_lowercase());
    store.agents.insert(
        key,
        HostedAgent {
            group_id: group_id.to_string(),
            name: name.trim().to_string(),
            token: token.to_string(),
        },
    );
    save_hosted_agents(&store)
}

pub(crate) fn validate_hosted_agent_token(group_id: &str, name: &str, token: &str) -> bool {
    let key = format!("{}:{}", group_id, name.trim().to_lowercase());
    if let Ok(store) = load_hosted_agents()
        && let Some(agent) = store.agents.get(&key)
    {
        return constant_time_token_eq(&agent.token, token);
    }
    false
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
        recompute_member_token_index(&mut group);
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
    let mut group: Group = serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{e}"))?;
    recompute_member_token_index(&mut group);
    Ok(group)
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
    let metadata = std::fs::metadata(&path)?;
    if metadata.len() > MAX_GROUP_MESSAGES_BYTES {
        bail!(
            "group {id} messages file exceeds the {} byte limit; clear or archive messages",
            MAX_GROUP_MESSAGES_BYTES
        );
    }
    let file = std::fs::OpenOptions::new().read(true).open(&path)?;
    file.lock_shared()?;
    let reader = std::io::BufReader::new(file);
    let mut messages: VecDeque<GroupMessage> = VecDeque::with_capacity(MAX_LOADED_MESSAGES);
    for line in reader.lines() {
        let line = line.with_context(|| format!("read message line in {}", path.display()))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let m: GroupMessage =
            serde_json::from_str(line).with_context(|| format!("parse message line: {line}"))?;
        if messages.len() >= MAX_LOADED_MESSAGES {
            messages.pop_front();
        }
        messages.push_back(m);
    }
    Ok(messages.into_iter().collect())
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
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    file.lock_exclusive()?;

    let line = serde_json::to_string(message)?;
    let line_len = line.len() as u64 + 1;
    let size = file.metadata()?.len();
    if size + line_len > MAX_GROUP_MESSAGES_BYTES {
        file.seek(SeekFrom::Start(0))?;
        let reader = std::io::BufReader::new(&file);
        let mut kept: VecDeque<String> = VecDeque::with_capacity(MAX_LOADED_MESSAGES);
        for l in reader.lines() {
            let l = l?;
            let l = l.trim();
            if l.is_empty() {
                continue;
            }
            if kept.len() >= MAX_LOADED_MESSAGES {
                kept.pop_front();
            }
            kept.push_back(l.to_string());
        }
        let drop_count = kept.len() / 2;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        for l in kept.into_iter().skip(drop_count) {
            writeln!(file, "{l}")?;
        }
        file.flush()?;
    }

    file.seek(SeekFrom::End(0))?;
    writeln!(file, "{line}")?;
    drop(file);
    if !existed {
        crate::providers::restrict_omg_file_permissions(&path)?;
    }
    Ok(())
}

pub(crate) async fn add_message_async(id: &str, message: &GroupMessage) -> Result<()> {
    if message.content.len() > MAX_GROUP_MESSAGE_BYTES {
        bail!("message too large (max {MAX_GROUP_MESSAGE_BYTES} bytes)");
    }
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
            let id = args.id.clone();
            let human_name = args
                .human_name
                .clone()
                .unwrap_or_else(default_human_name)
                .trim()
                .to_string();
            let provided_token = args.token.clone();
            if let Some(remote) = args.remote.as_deref() {
                let token =
                    resolve_remote_member_token(&id, &human_name, provided_token.as_deref())?;
                let validated = validate_remote_base_url(remote).await?;
                chat_remote(&id, &token, &human_name, &validated).await
            } else {
                let name_for_token = human_name.clone();
                let id_for_token = id.clone();
                let (token, human_name) = modify_group_async(&id, move |g| {
                    resolve_local_member_token(
                        g,
                        &id_for_token,
                        &name_for_token,
                        provided_token.as_deref(),
                    )
                })
                .await?;
                chat(&id, &token, &human_name, args.yolo).await
            }
        }
        GroupCommand::Send(args) => {
            let id = args.id.clone();
            let human_name = args
                .human_name
                .clone()
                .unwrap_or_else(default_human_name)
                .trim()
                .to_string();
            let content = args.message.trim().to_string();
            if content.len() > MAX_GROUP_MESSAGE_BYTES {
                bail!("message too large (max {MAX_GROUP_MESSAGE_BYTES} bytes)");
            }
            let mut message = GroupMessage {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                sender: human_name.clone(),
                content,
                kind: MessageKind::Human,
            };
            let provided_token = args.token.clone();
            if let Some(remote) = args.remote.as_deref() {
                let token =
                    resolve_remote_member_token(&id, &human_name, provided_token.as_deref())?;
                let validated = validate_remote_base_url(remote).await?;
                send_remote(&id, &token, &message, &validated).await?;
                save_membership(&id, &human_name, &token)?;
                println!("sent message to remote group {}", id);
                Ok(())
            } else {
                let name_for_token = human_name.clone();
                let id_for_token = id.clone();
                let (token, canonical_name) = modify_group_async(&id, move |g| {
                    resolve_local_member_token(
                        g,
                        &id_for_token,
                        &name_for_token,
                        provided_token.as_deref(),
                    )
                })
                .await?;
                message.sender = canonical_name;
                send(&id, &token, &message).await
            }
        }
        GroupCommand::Join(args) => {
            if let Some(remote) = args.remote.as_deref() {
                let token = args
                    .token
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("--token is required for remote group join"))?;
                let validated = validate_remote_base_url(remote).await?;
                join_remote(&args.id, token, args, &validated).await
            } else {
                join_local(&args.id, args).await
            }
        }
        GroupCommand::Approve(args) => {
            let approver = args
                .name
                .clone()
                .unwrap_or_else(default_human_name)
                .trim()
                .to_string();
            if let Some(remote) = args.remote.as_deref() {
                let token =
                    resolve_remote_member_token(&args.id, &approver, args.token.as_deref())?;
                let validated = validate_remote_base_url(remote).await?;
                approve_remote(&args.id, &args.request_id, &token, args, &validated).await
            } else {
                approve_local(&args.id, &args.request_id, args).await
            }
        }
        GroupCommand::Invite { id } => invite(id),
        GroupCommand::RemoteAgentAdd(args) => add_remote_agent(&args.id, args).await,
        GroupCommand::RemoteAgentList { id } => list_remote_agents(id).await,
        GroupCommand::RemoteAgentRemove { id, name } => remove_remote_agent(id, name).await,
        GroupCommand::HostAgent(args) => host_agent(args).await,
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

    let mut member_tokens = HashMap::new();
    let mut member_token_index = HashMap::new();
    let host_token = generate_member_token();
    member_tokens.insert(spec.host_name.clone(), host_token.clone());
    member_token_index.insert(host_token.clone(), spec.host_name.clone());

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
        member_tokens,
        member_token_index,
        pending_joins: Vec::new(),
        agents,
        remote_agents: Vec::new(),
        approved_member_tokens: HashMap::new(),
    };

    save_group(&group)?;
    save_membership(&group.id, &group.host_name, &host_token)?;
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
    if group
        .remote_agents
        .iter()
        .any(|r| r.name.eq_ignore_ascii_case(n))
    {
        bail!("human name '{n}' conflicts with a remote agent name");
    }
    Ok(())
}

pub(crate) fn ensure_local_member(group: &mut Group, name: &str) -> Result<String> {
    validate_member_name(group, name)?;
    issue_member_token(group, name)
}

pub(crate) fn truncate_message_content(s: &str) -> String {
    let mut out = String::with_capacity(MAX_GROUP_MESSAGE_BYTES.min(s.len()));
    let mut len = 0;
    for c in s.chars() {
        let cl = c.len_utf8();
        if len + cl > MAX_GROUP_MESSAGE_BYTES {
            break;
        }
        out.push(c);
        len += cl;
    }
    out
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
        pre_auth_token: Some(generate_member_token()),
    });
    Ok(id)
}

pub(crate) fn approve_join_request(
    group: &mut Group,
    request_id: &str,
    pre_auth: &str,
) -> Result<(String, String)> {
    let pos = group
        .pending_joins
        .iter()
        .position(|r| r.id == request_id)
        .ok_or_else(|| anyhow::anyhow!("join request {request_id} not found"))?;
    let req = group.pending_joins.remove(pos);
    if let Some(expected) = &req.pre_auth_token
        && !pre_auth.is_empty()
        && !constant_time_token_eq(pre_auth, expected)
    {
        bail!("pre-auth token mismatch for join request {request_id}");
    }
    let token = issue_member_token(group, &req.name)?;
    if !pre_auth.is_empty() {
        group
            .approved_member_tokens
            .insert(pre_auth.to_string(), token.clone());
    }
    Ok((req.name, token))
}

async fn new_group(args: &GroupNewArgs) -> Result<()> {
    let group = create_group(args).await?;
    let host_token = group
        .member_tokens
        .get(&group.host_name)
        .cloned()
        .unwrap_or_else(generate_member_token);

    println!("created group {}: {}", group.id, group.name);
    println!("  agents:");
    for a in &group.agents {
        println!("    {} ({}) — {}", a.name, a.model, a.role);
    }
    println!("\nhost member token: {host_token}");
    println!(
        "host chat:    omgb group chat {} --token {}",
        group.id, host_token
    );
    println!(
        "send message: omgb group send {} \"<message>\" --token {}",
        group.id, host_token
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
    if !group.remote_agents.is_empty() {
        println!("remote agents:");
        for r in &group.remote_agents {
            println!("  {} ({}) — {}", r.name, r.model, r.role);
        }
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
        "share this invite link with humans/agents to join group {}:\n",
        group.name
    );
    println!("  omgb://group/{id}?token={}", group.invite_token);
    if let Ok(remote) = std::env::var("OMGB_REMOTE") {
        let remote = remote.trim_end_matches('/');
        println!("  {remote}/group/{id}?token={}", group.invite_token);
    }
    Ok(())
}

async fn add_remote_agent(id: &str, args: &GroupRemoteAgentAddArgs) -> Result<()> {
    crate::threads::validate_id(id)?;
    let name = args.name.trim().to_string();
    validate_human_name(&name)?;
    let url = args.url.trim().to_string();
    if url.is_empty() {
        bail!("remote agent url is required");
    }
    let token = args
        .token
        .as_deref()
        .map(|s: &str| s.trim().to_string())
        .filter(|s: &String| !s.is_empty())
        .unwrap_or_else(generate_member_token);
    let role = args.role.trim().to_string();
    let model = normalize_model(args.model.trim());
    let allow_local = args.allow_local;

    validate_remote_agent_url(&url, allow_local).await?;

    let name_print = name.clone();
    let url_print = url.clone();
    let token_print = token.clone();
    modify_group_async(id, move |group| {
        if all_agent_names(group).len() >= MAX_AGENTS {
            bail!("a group can have at most {MAX_AGENTS} agents");
        }
        if group
            .agents
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case(&name))
            || group
                .remote_agents
                .iter()
                .any(|r| r.name.eq_ignore_ascii_case(&name))
            || group.members.iter().any(|m| m.eq_ignore_ascii_case(&name))
        {
            bail!("name '{name}' is already in use in this group");
        }
        group.remote_agents.push(RemoteAgent {
            name: name.clone(),
            role: role.clone(),
            model: model.clone(),
            token: token.clone(),
            callback_url: Some(url.clone()),
            allow_local,
            last_heartbeat: None,
        });
        Ok(())
    })
    .await?;
    println!("added remote agent '{name_print}' to group {id}");
    println!("  url: {url_print}");
    println!("  token: {token_print}");
    Ok(())
}

async fn list_remote_agents(id: &str) -> Result<()> {
    crate::threads::validate_id(id)?;
    let group = load_group_async(id).await?;
    if group.remote_agents.is_empty() {
        println!("no remote agents in group {id}");
        return Ok(());
    }
    println!("remote agents in group {}:", group.name);
    for r in &group.remote_agents {
        let status = r
            .last_heartbeat
            .map(|h| format!("last heartbeat {h}"))
            .unwrap_or_else(|| "never seen".to_string());
        let url = r.callback_url.as_deref().unwrap_or("-");
        println!("  {} ({}) — {} [{}]", r.name, r.role, url, status);
    }
    Ok(())
}

async fn remove_remote_agent(id: &str, name: &str) -> Result<()> {
    crate::threads::validate_id(id)?;
    let name = name.trim().to_string();
    let name_print = name.clone();
    let id_owned = id.to_string();
    modify_group_async(id, move |group| {
        let pos = group
            .remote_agents
            .iter()
            .position(|r| r.name.eq_ignore_ascii_case(&name))
            .ok_or_else(|| {
                anyhow::anyhow!("remote agent '{name}' not found in group {id_owned}")
            })?;
        group.remote_agents.remove(pos);
        Ok(())
    })
    .await?;
    println!("removed remote agent '{name_print}' from group {id}");
    Ok(())
}

async fn host_agent(args: &GroupHostAgentArgs) -> Result<()> {
    let group_id = args.id.trim().to_string();
    crate::threads::validate_id(&group_id)?;
    let name = args.name.trim().to_string();
    validate_human_name(&name)?;
    let token = args
        .token
        .as_deref()
        .map(|s: &str| s.trim().to_string())
        .filter(|s: &String| !s.is_empty())
        .unwrap_or_else(generate_member_token);
    let group_id_print = group_id.clone();
    let name_print = name.clone();
    let token_print = token.clone();
    tokio::task::spawn_blocking(move || register_hosted_agent(&group_id, &name, &token))
        .await
        .context("register hosted agent task failed")??;
    println!("registered hosted agent '{name_print}' for group {group_id_print}");
    println!("  dispatch URL path: /group/{group_id_print}/agent/{name_print}/dispatch");
    println!("  token: {token_print}");
    println!(
        "  configure the remote group with: omgb group remote-agent-add {group_id_print} {name_print} --url <this-server>/group/{group_id_print}/agent/{name_print}/dispatch --token {token_print}"
    );
    Ok(())
}

async fn send(id: &str, _token: &str, message: &GroupMessage) -> Result<()> {
    if message.content.len() > MAX_GROUP_MESSAGE_BYTES {
        bail!("message too large (max {MAX_GROUP_MESSAGE_BYTES} bytes)");
    }
    let group = load_group_async(id).await?;
    add_message_async(&group.id, message).await?;
    let trigger_for_dispatch = message.clone();
    let sender = trigger_for_dispatch.sender.clone();
    let group_for_dispatch = group.clone();
    let runtime_handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = runtime_handle.block_on(dispatch_for_message(
            group_for_dispatch,
            trigger_for_dispatch,
            sender,
        )) {
            eprintln!("warning: group dispatch failed: {e}");
        }
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
    println!(
        "Approve with: omgb group approve <id> <request_id> --token <member-token> [--remote <url>]"
    );
    println!("\x1b[0m");
}

async fn chat(id: &str, _token: &str, human_name: &str, yolo: bool) -> Result<()> {
    let group = load_group_async(id).await?;
    let yolo = yolo || group.yolo;

    println!("group: {} ({})", group.name, group.id);
    println!("agents: {}", all_agent_names(&group).join(", "));
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
                        dispatch_turn(&group, m, human_name, yolo, &mut seen).await?;
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
                if text.len() > MAX_GROUP_MESSAGE_BYTES {
                    eprintln!("message too large (max {MAX_GROUP_MESSAGE_BYTES} bytes)");
                    continue;
                }

                let message = GroupMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    timestamp: Utc::now(),
                    sender: human_name.to_string(),
                    content: text.to_string(),
                    kind: MessageKind::User,
                };
                add_message_async(&group.id, &message).await?;
                print_message(&message);
                seen.insert(message.id.clone());
                dispatch_turn(&group, &message, human_name, yolo, &mut seen).await?;
            }
        }
    }

    println!("\nleft group {}", group.id);
    Ok(())
}

#[derive(serde::Deserialize)]
struct RemoteGroupAgentInfo {
    name: String,
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
    #[serde(default)]
    remote_agents: Vec<RemoteGroupAgentInfo>,
}

pub(crate) async fn chat_remote(
    id: &str,
    token: &str,
    human_name: &str,
    vurl: &crate::net::ValidatedUrl,
) -> Result<()> {
    let base = vurl.url.as_str().trim_end_matches('/');
    let info_url = format!("{base}/group/{id}");
    let messages_url = format!("{base}/group/{id}/messages");
    let joins_url = format!("{base}/group/{id}/joins");

    let client = crate::net::build_client(vurl, std::time::Duration::from_secs(30))?;
    let info = match client
        .get(&info_url)
        .header("x-member-token", token)
        .send()
        .await
    {
        Ok(res) if res.status().is_success() => res
            .json::<RemoteGroupInfo>()
            .await
            .map_err(|e| anyhow::anyhow!("failed to parse group info: {e}"))?,
        Ok(res) => bail!("failed to fetch group info: {}", res.status()),
        Err(e) => bail!("failed to fetch group info: {e}"),
    };

    let group = Group {
        id: id.to_string(),
        name: info.name,
        description: info.description,
        created_at: Utc::now(),
        model: info.model,
        yolo: info.yolo,
        invite_token: String::new(),
        host_name: info.host_name,
        members: info.members,
        member_tokens: HashMap::new(),
        member_token_index: HashMap::new(),
        pending_joins: info.pending_joins,
        agents: info.agents,
        remote_agents: Vec::new(),
        approved_member_tokens: HashMap::new(),
    };

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
    if !info.remote_agents.is_empty() {
        println!(
            "remote agents: {}",
            info.remote_agents
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!("members: {}", group.members.join(", "));
    if !group.pending_joins.is_empty() {
        print_pending_alert(&group.pending_joins);
    }
    println!("type a message and press Enter. /quit or /exit to leave.\n");

    let mut seen: HashSet<String> = HashSet::new();
    match client
        .get(&messages_url)
        .header("x-member-token", token)
        .send()
        .await
    {
        Ok(res) if res.status().is_success() => {
            save_membership(id, human_name, token)?;
            if let Ok(initial) = res.json::<Vec<GroupMessage>>().await {
                for m in &initial {
                    print_message(m);
                    seen.insert(m.id.clone());
                }
            }
        }
        Ok(res) if res.status().as_u16() == 401 => {
            bail!("invalid member token for group {id}")
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
                match client.get(&messages_url).header("x-member-token", token).send().await {
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

                match client.get(&joins_url).header("x-member-token", token).send().await {
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
                if text.len() > MAX_GROUP_MESSAGE_BYTES {
                    eprintln!("message too large (max {MAX_GROUP_MESSAGE_BYTES} bytes)");
                    continue;
                }

                let message = GroupMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    timestamp: Utc::now(),
                    sender: human_name.to_string(),
                    content: text.to_string(),
                    kind: MessageKind::User,
                };
                if let Err(e) = send_remote(id, token, &message, vurl).await {
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
    vurl: &crate::net::ValidatedUrl,
) -> Result<()> {
    if message.content.len() > MAX_GROUP_MESSAGE_BYTES {
        bail!("message too large (max {MAX_GROUP_MESSAGE_BYTES} bytes)");
    }
    let base = vurl.url.as_str().trim_end_matches('/');
    let url = format!("{base}/group/{id}/messages");
    let client = crate::net::build_client(vurl, std::time::Duration::from_secs(30))?;
    let res = client
        .post(&url)
        .header("x-member-token", token)
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
        n.trim().to_string()
    } else {
        read_line_prompt("Enter your name: ").await?
    };
    if name.is_empty() {
        bail!("name is required to join a group");
    }
    let github = args.github.clone();
    let modify_name = name.clone();
    let (status, request_id, token) = modify_group_async(id, move |group| {
        if group.members.is_empty() {
            let token = ensure_local_member(group, &modify_name)?;
            return Ok(("approved".to_string(), String::new(), token));
        }
        if is_member(group, &modify_name) {
            let token = issue_member_token(group, &modify_name)?;
            return Ok(("approved".to_string(), String::new(), token));
        }
        let request_id = add_join_request(group, &modify_name, github.as_deref())?;
        Ok(("pending".to_string(), request_id, String::new()))
    })
    .await?;
    if status == "approved" {
        save_membership(id, &name, &token)?;
        println!("'{name}' joined group {id} (member token: {token})");
    } else {
        println!("join request {request_id} for '{name}' is pending approval in group {id}");
        println!(
            "an existing member can approve with: omgb group approve {id} {request_id} --token <member-token>"
        );
    }
    Ok(())
}

async fn approve_local(id: &str, request_id: &str, args: &GroupApproveArgs) -> Result<()> {
    let approver = args
        .name
        .clone()
        .unwrap_or_else(default_human_name)
        .trim()
        .to_string();
    let token = args
        .token
        .as_deref()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("--token <member-token> is required to approve a join request")
        })?;
    let request_id = request_id.to_string();
    let modify_request_id = request_id.clone();
    let token = token.to_string();
    let (name, member_token) = modify_group_async(id, move |group| {
        if let Some(member_name) = validate_member_token(group, &token) {
            if !name_eq(&approver, &member_name) {
                bail!("token belongs to member '{member_name}', not '{approver}'");
            }
        } else {
            bail!("invalid member token");
        }
        approve_join_request(group, &modify_request_id, "")
    })
    .await?;
    println!("approved join request {request_id}: '{name}' can now post in group {id}");
    println!("member token for '{name}': {member_token}");
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JoinResult {
    pub id: String,
    pub status: String,
    pub name: String,
    pub github: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_auth_token: Option<String>,
}

async fn join_remote(
    id: &str,
    token: &str,
    args: &GroupJoinArgs,
    vurl: &crate::net::ValidatedUrl,
) -> Result<()> {
    let name = if let Some(n) = args.name.as_deref() {
        n.trim().to_string()
    } else {
        read_line_prompt("Enter your name: ").await?
    };
    if name.is_empty() {
        bail!("name is required to join a group");
    }
    let base = vurl.url.as_str().trim_end_matches('/');
    let url = format!("{base}/group/{id}/join");
    let github = args
        .github
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let client = crate::net::build_client(vurl, std::time::Duration::from_secs(30))?;
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
    match result.status.as_str() {
        "approved" => {
            if let Some(member_token) = result.member_token {
                save_membership(id, &name, &member_token)?;
                println!(
                    "'{name}' was auto-approved for group {id} (member token: {member_token})"
                );
            } else {
                println!("'{name}' was auto-approved for group {id}");
            }
        }
        "member" => {
            if let Some(membership) = load_membership_by_name(id, &name) {
                println!(
                    "'{name}' is already a member of group {id} (member token: {membership_token})",
                    membership_token = membership.token
                );
            } else {
                bail!(
                    "'{name}' is already a member of group {id}; pass --token <member-token> to chat"
                );
            }
        }
        _ => {
            println!(
                "join request {} for '{name}' is pending approval in group {id}",
                result.id
            );
            println!(
                "an existing member can approve with: omgb group approve {id} {} --token <token> --remote {base}",
                result.id
            );
            if let Some(pre_auth) = result.pre_auth_token {
                println!(
                    "poll for approval with: GET {base}/group/{id}/joins/{}/status?pre_auth={pre_auth}",
                    result.id
                );
            }
        }
    }
    Ok(())
}

async fn approve_remote(
    id: &str,
    request_id: &str,
    token: &str,
    _args: &GroupApproveArgs,
    vurl: &crate::net::ValidatedUrl,
) -> Result<()> {
    let base = vurl.url.as_str().trim_end_matches('/');
    let url = format!("{base}/group/{id}/joins/{request_id}/approve");
    let client = crate::net::build_client(vurl, std::time::Duration::from_secs(30))?;
    let res = client
        .post(&url)
        .header("x-member-token", token)
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
    if let Some(member_token) = result.member_token {
        println!("member token for '{}': {member_token}", result.name);
    }
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
    let pre_main_seen = seen.clone();

    let agent_names = all_agent_names(group);
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

    let replies: Vec<_> = parse_replies(&reply, &agent_names)
        .into_iter()
        .filter(|(name, _)| !name.eq_ignore_ascii_case(&trigger.sender))
        .collect();

    let mut new_message_ids: Vec<String> = Vec::new();
    for (name, _content) in replies {
        let content = if let Some(remote) = find_remote_agent(group, &name) {
            let prompt = format!(
                "You are {} ({}) in the group chat.\n\n\
                 Conversation so far:\n{}\n\n\
                 Reply to the latest message. Be concise and in character.",
                remote.name,
                remote.role,
                format_history(&messages, HISTORY_LIMIT)
            );
            match dispatch_remote_agent(group, remote, &prompt, &messages, trigger).await {
                Some(c) => c,
                None => continue,
            }
        } else if let Some(agent) = group
            .agents
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(&name))
        {
            let prompt = format!(
                "You are {} ({}) in the group chat.\n\n\
                 Conversation so far:\n{}\n\n\
                 Reply to the latest message. Be concise and in character.",
                agent.name,
                agent.role,
                format_history(&messages, HISTORY_LIMIT)
            );
            let model = agent_model(group, &agent.name);
            match crate::run_single_turn_capture(&prompt, Some(model), yolo, Some(1), None).await {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("warning: agent {name} reply failed: {e}");
                    continue;
                }
            }
        } else {
            continue;
        };
        let content = truncate_message_content(&content);
        let trimmed = content.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("NO_REPLY") {
            continue;
        }
        let message = GroupMessage {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            sender: name,
            content: trimmed.into(),
            kind: MessageKind::Agent,
        };
        add_message_async(&group.id, &message).await?;
        messages.push(message.clone());
        print_message(&message);
        new_message_ids.push(message.id.clone());
    }

    // One round of @mention routing so agents can ask each other directly.
    // Only consider messages produced in this turn so the same mention pair
    // does not trigger repeatedly.
    let mut mention_targets: Vec<(String, String, String)> = Vec::new();
    for m in messages.iter().rev().take(HISTORY_LIMIT) {
        if matches!(m.kind, MessageKind::Agent) && !pre_main_seen.contains(&m.id) {
            for target in parse_mentions(&m.content, &agent_names) {
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
        let content = if let Some(remote) = find_remote_agent(group, &target_name) {
            let prompt = format!(
                "You are {} ({}) in the group chat.\n\n\
                 Conversation so far:\n{}\n\n\
                 {} mentioned you:\n{}\n\n\
                 Reply concisely. Do not ask follow-up questions unless essential.",
                remote.name,
                remote.role,
                format_history(&messages, HISTORY_LIMIT),
                sender,
                context
            );
            match dispatch_remote_agent(group, remote, &prompt, &messages, trigger).await {
                Some(c) => c,
                None => continue,
            }
        } else if let Some(agent) = group
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
            match crate::run_single_turn_capture(&prompt, Some(model), yolo, Some(1), None).await {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("warning: mention reply failed: {e}");
                    continue;
                }
            }
        } else {
            continue;
        };
        let content = truncate_message_content(&content);
        let trimmed = content.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("NO_REPLY") {
            continue;
        }
        let message = GroupMessage {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            sender: target_name,
            content: trimmed.into(),
            kind: MessageKind::Agent,
        };
        add_message_async(&group.id, &message).await?;
        messages.push(message.clone());
        print_message(&message);
        new_message_ids.push(message.id.clone());
    }

    for id in new_message_ids {
        seen.insert(id);
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

fn all_agent_names(group: &Group) -> Vec<String> {
    let mut names: Vec<String> = group.agents.iter().map(|a| a.name.clone()).collect();
    for r in &group.remote_agents {
        if !names.iter().any(|n| n.eq_ignore_ascii_case(&r.name)) {
            names.push(r.name.clone());
        }
    }
    names
}

fn find_remote_agent<'a>(group: &'a Group, name: &str) -> Option<&'a RemoteAgent> {
    group
        .remote_agents
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(name))
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
    for r in &group.remote_agents {
        prompt.push_str(&format!(
            "- {} (role: {}, model: {}, remote host)\n",
            r.name, r.role, r.model
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

fn parse_replies(text: &str, agent_names: &[String]) -> Vec<(String, String)> {
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
            if let Some(matched) = agent_names.iter().find(|n| n.eq_ignore_ascii_case(name)) {
                out.push((matched.clone(), content.to_string()));
            }
        }
    }
    out
}

fn parse_mentions(text: &str, agent_names: &[String]) -> Vec<String> {
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
            for n in agent_names {
                if seen.insert(n.to_lowercase()) {
                    out.push(n.clone());
                }
            }
        } else if let Some(matched) = agent_names.iter().find(|n| n.eq_ignore_ascii_case(name))
            && seen.insert(matched.to_lowercase())
        {
            out.push(matched.clone());
        }
    }
    out
}

async fn dispatch_remote_agent(
    group: &Group,
    remote: &RemoteAgent,
    prompt: &str,
    history: &[GroupMessage],
    trigger: &GroupMessage,
) -> Option<String> {
    let url = remote.callback_url.as_deref()?;
    if url.trim().is_empty() {
        return None;
    }
    let vurl = validate_remote_agent_url(url, remote.allow_local)
        .await
        .ok()?;

    let payload = RemoteAgentDispatchPayload {
        group_id: group.id.clone(),
        agent_name: remote.name.clone(),
        role: remote.role.clone(),
        model: remote.model.clone(),
        group_model: normalize_model(&group.model),
        yolo: group.yolo,
        prompt: prompt.to_string(),
        history: history
            .iter()
            .rev()
            .take(HISTORY_LIMIT)
            .rev()
            .cloned()
            .collect(),
        message: trigger.clone(),
    };

    let mut headers = std::collections::HashMap::new();
    headers.insert("x-agent-token".to_string(), remote.token.clone());
    match crate::net::http_post_json(
        &vurl,
        &headers,
        serde_json::to_value(payload).ok()?,
        std::time::Duration::from_secs(120),
    )
    .await
    {
        Ok((200, text)) => serde_json::from_str::<RemoteAgentDispatchResponse>(&text)
            .ok()
            .and_then(|r| {
                let c = truncate_message_content(&r.content).trim().to_string();
                if c.is_empty() || c.eq_ignore_ascii_case("NO_REPLY") {
                    None
                } else {
                    Some(c)
                }
            }),
        Ok((status, text)) => {
            let preview: String = text.chars().take(120).collect();
            eprintln!(
                "warning: remote agent {} returned HTTP {status}: {preview}",
                remote.name
            );
            None
        }
        Err(e) => {
            eprintln!("warning: remote agent {} unreachable: {e}", remote.name);
            None
        }
    }
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

fn name_eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

async fn validate_remote_agent_url(
    url: &str,
    allow_local: bool,
) -> Result<crate::net::ValidatedUrl> {
    // --allow-local gates both loopback and private/LAN agent callbacks.
    crate::net::validate_url(url, allow_local, allow_local).await
}

async fn validate_remote_base_url(remote: &str) -> Result<crate::net::ValidatedUrl> {
    let remote = remote.trim_end_matches('/');
    let is_loopback = crate::net::is_url_host_loopback(remote);
    let is_private = crate::net::is_url_host_private(remote).await;
    crate::net::validate_url(remote, is_loopback, is_private).await
}

/// Resolve a member token for a local group, validating the provided token or
/// falling back to a saved membership. Returns `(token, canonical_member_name)`.
fn resolve_local_member_token(
    group: &mut Group,
    group_id: &str,
    human_name: &str,
    provided_token: Option<&str>,
) -> Result<(String, String)> {
    let human_name = human_name.trim().to_string();
    validate_member_name(group, &human_name)?;
    if !is_member(group, &human_name) {
        bail!("'{human_name}' is not a member of group {group_id}; request to join first");
    }
    if let Some(t) = provided_token.filter(|t| !t.is_empty()) {
        if let Some(name) = validate_member_token(group, t) {
            if name_eq(&human_name, &name) {
                save_membership(group_id, &human_name, t)?;
                return Ok((t.to_string(), name));
            }
            bail!("token belongs to member '{name}', not '{human_name}'");
        }
        bail!("invalid member token for group {group_id}");
    }
    if let Some(membership) = load_membership_by_name(group_id, &human_name)
        && let Some(name) = validate_member_token(group, &membership.token)
        && name_eq(&human_name, &name)
    {
        return Ok((membership.token, name));
    }
    bail!(
        "no member token found for '{human_name}' in group {group_id}; pass --token <member-token> or run `omgb group join` first"
    )
}

/// Resolve a member token for a remote group without requiring the local group file.
fn resolve_remote_member_token(
    group_id: &str,
    human_name: &str,
    provided_token: Option<&str>,
) -> Result<String> {
    let human_name = human_name.trim().to_string();
    validate_human_name(&human_name)?;
    if let Some(t) = provided_token.filter(|t| !t.is_empty()) {
        return Ok(t.to_string());
    }
    if let Some(membership) = load_membership_by_name(group_id, &human_name) {
        return Ok(membership.token);
    }
    bail!(
        "no member token found for '{human_name}' in group {group_id}; pass --token <member-token> or run `omgb group join` first"
    )
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
        let names = vec!["Alice".to_string(), "bob".to_string()];
        let m = parse_mentions("@alice can you check this? @ALL @unknown", &names);
        assert_eq!(m, vec!["Alice".to_string(), "bob".to_string()]);

        let m = parse_mentions("email me at alice@example.com or use array@index", &names);
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
        let names = vec!["Alpha".to_string()];
        let text = r#"{"replies":{"Alpha":"working on it","Beta":"NO_REPLY"}}"#;
        let replies = parse_replies(text, &names);
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

    #[test]
    fn truncate_message_content_respects_byte_limit_and_char_boundaries() {
        let short = "hello";
        assert_eq!(truncate_message_content(short), "hello");

        let repeated = "a".repeat(MAX_GROUP_MESSAGE_BYTES + 10);
        let truncated = truncate_message_content(&repeated);
        assert_eq!(truncated.len(), MAX_GROUP_MESSAGE_BYTES);

        let multi_byte = "🎉".repeat(2000);
        let truncated = truncate_message_content(&multi_byte);
        assert!(truncated.len() <= MAX_GROUP_MESSAGE_BYTES);
        assert!(!truncated.is_empty());
    }
}
