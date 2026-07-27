//! Multi-agent thread orchestration for `omgb`.
//!
//! Threads are persistent sessions visible across the workspace. Any agent can
//! create a thread, list threads, peek at output, prompt them, open them in the
//! TUI, send messages between threads, and pick the best model for a task.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::args::{
    ThreadArgs, ThreadCommand, ThreadNewArgs, ThreadPeekArgs, ThreadPromptArgs, ThreadSendArgs,
};
use crate::{SessionParams, TuiArgs, run_single_turn_with, run_tui};
use xai_grok_pager::headless::OutputFormat;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadMessage {
    pub from: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadRecord {
    pub id: String,
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub last_message_at: DateTime<Utc>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inbox: Vec<ThreadMessage>,
}

fn threads_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("threads.jsonl"))
}

fn lock_path() -> Result<PathBuf> {
    Ok(crate::providers::omg_dir()?.join("threads.lock"))
}

fn load_records_unlocked() -> Result<Vec<ThreadRecord>> {
    let path = threads_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| anyhow::anyhow!("{}: {e}", path.display())))
        .collect()
}

fn save_records_unlocked(records: &[ThreadRecord]) -> Result<()> {
    let path = threads_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("threads path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut lines = String::new();
    for r in records {
        lines.push_str(&serde_json::to_string(r)?);
        lines.push('\n');
    }
    crate::providers::write_file_atomic(&path, lines.as_bytes(), true)
        .with_context(|| format!("write {}", path.display()))
}

fn with_records<T>(f: impl FnOnce(Vec<ThreadRecord>) -> Result<T>) -> Result<T> {
    let lock = lock_path()?;
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock)?;
    file.lock_shared()?;
    let records = load_records_unlocked()?;
    let result = f(records);
    drop(file);
    result
}

fn with_records_mut<T>(f: impl FnOnce(&mut Vec<ThreadRecord>) -> Result<T>) -> Result<T> {
    let lock = lock_path()?;
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock)?;
    file.lock_exclusive()?;
    let mut records = load_records_unlocked()?;
    let result = f(&mut records);
    if result.is_ok() {
        save_records_unlocked(&records)?;
    }
    drop(file);
    result
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn validate_id(id: &str) -> Result<()> {
    if !is_safe_id(id) {
        bail!("invalid thread id '{id}'");
    }
    Ok(())
}

fn chat_history_path(record: &ThreadRecord) -> Option<PathBuf> {
    xai_grok_shell::session::persistence::find_session_dir_by_id(&record.session_id)
        .map(|p| p.join("chat_history.jsonl"))
}

/// Returns the text of the last assistant message in the thread's session.
pub(crate) async fn last_assistant_text(thread_id: &str) -> Result<String> {
    let session_id = with_records(|records| {
        records
            .into_iter()
            .find(|r| r.id == thread_id)
            .map(|r| r.session_id)
            .ok_or_else(|| anyhow::anyhow!("thread '{thread_id}' not found"))
    })?;
    let path = xai_grok_shell::session::persistence::find_session_dir_by_id(&session_id)
        .ok_or_else(|| anyhow::anyhow!("session for thread '{thread_id}' not found"))?
        .join("chat_history.jsonl");
    if !path.is_file() {
        bail!("thread '{thread_id}' has no chat history");
    }
    let raw = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    let mut text = String::new();
    for line in raw.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        if let Ok(xai_grok_shell::sampling::ConversationItem::Assistant(a)) =
            serde_json::from_str::<xai_grok_shell::sampling::ConversationItem>(line)
        {
            text = a.content.as_ref().to_string();
        }
    }
    if text.is_empty() {
        bail!("no assistant response in thread '{thread_id}'");
    }
    Ok(text)
}

pub async fn run_thread(args: ThreadArgs) -> Result<()> {
    match args.command {
        ThreadCommand::New(args) => run_new(args).await,
        ThreadCommand::List => list(),
        ThreadCommand::Prompt(args) => run_prompt(args).await,
        ThreadCommand::Chat { id } => run_chat(&id).await,
        ThreadCommand::Peek(args) => run_peek(args),
        ThreadCommand::Send(args) => run_send(args),
        ThreadCommand::Inbox { id } => run_inbox(&id),
        ThreadCommand::Models => list_models().await,
        ThreadCommand::PickModel { task } => pick_model(&task).await,
    }
}

pub async fn create(
    prompt: &str,
    model: Option<String>,
    yolo: bool,
    requested_id: Option<String>,
) -> Result<(String, String)> {
    let model = match model {
        Some(m) => m,
        None => pick_model_for_task(prompt).await?,
    };
    let cwd = std::env::current_dir()?.to_string_lossy().to_string();
    let summary = prompt.lines().next().unwrap_or(prompt).to_string();
    let (id, session_id): (String, String) = with_records_mut(|records| {
        let id = match requested_id {
            Some(id) => {
                validate_id(&id)?;
                if records.iter().any(|r| r.id == id) {
                    bail!("thread '{id}' already exists");
                }
                id
            }
            None => uuid::Uuid::new_v4().to_string(),
        };
        let session_id = uuid::Uuid::new_v4().to_string();
        records.push(ThreadRecord {
            id: id.clone(),
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            model: model.clone(),
            created_at: Utc::now(),
            last_message_at: Utc::now(),
            summary: summary.clone(),
            inbox: Vec::new(),
        });
        Ok((id, session_id))
    })?;
    let _ = crate::notifications::push(
        "thread_created",
        serde_json::json!({"thread_id": id, "summary": summary}),
    );
    let session = SessionParams {
        session_id: Some(session_id),
        ..Default::default()
    };
    run_single_turn_with(
        prompt,
        Some(model.clone()),
        yolo,
        OutputFormat::Plain,
        None,
        None,
        None,
        None,
        None,
        &session,
        false,
    )
    .await?;
    Ok((id, model))
}

async fn run_new(args: ThreadNewArgs) -> Result<()> {
    let (id, _) = create(&args.prompt, args.model, args.yolo, args.id).await?;
    println!("created thread {id}");
    Ok(())
}

pub async fn prompt(id: &str, prompt_text: &str, model: Option<String>, yolo: bool) -> Result<()> {
    let explicit = model;
    let (session_id, model): (String, String) = with_records(|records| {
        let record = records
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("thread '{id}' not found"))?;
        let model = explicit.unwrap_or_else(|| record.model.clone());
        Ok((record.session_id.clone(), model))
    })?;
    let session = SessionParams {
        session_id: Some(session_id),
        ..Default::default()
    };
    run_single_turn_with(
        prompt_text,
        Some(model.clone()),
        yolo,
        OutputFormat::Plain,
        None,
        None,
        None,
        None,
        None,
        &session,
        false,
    )
    .await?;
    with_records_mut(|records| {
        let record = records
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("thread '{id}' not found"))?;
        record.last_message_at = Utc::now();
        record.model = model;
        Ok(())
    })?;
    Ok(())
}

async fn run_prompt(args: ThreadPromptArgs) -> Result<()> {
    prompt(&args.id, &args.prompt, args.model, args.yolo).await
}

fn list() -> Result<()> {
    with_records(|mut records| {
        if records.is_empty() {
            println!("no threads");
            return Ok(());
        }
        records.sort_by(|a, b| b.last_message_at.cmp(&a.last_message_at));
        for r in records {
            let summary = r.summary.lines().next().unwrap_or("");
            println!(
                "{} (model: {}, cwd: {})\n  last: {}  summary: {}",
                r.id,
                r.model,
                r.cwd,
                r.last_message_at.format("%Y-%m-%d %H:%M UTC"),
                summary
            );
        }
        Ok(())
    })
}

async fn run_chat(id: &str) -> Result<()> {
    let record: ThreadRecord = with_records(|records| {
        records
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("thread '{id}' not found"))
    })?;
    run_tui(TuiArgs {
        prompt: None,
        model: Some(record.model),
        session: SessionParams {
            session_id: Some(record.session_id),
            ..Default::default()
        },
    })
    .await
}

fn run_peek(args: ThreadPeekArgs) -> Result<()> {
    let record: ThreadRecord = with_records(|records| {
        records
            .into_iter()
            .find(|r| r.id == args.id)
            .ok_or_else(|| anyhow::anyhow!("thread '{}' not found", args.id))
    })?;
    let Some(path) = chat_history_path(&record) else {
        bail!("thread '{}' has no chat history yet", args.id);
    };
    if !path.is_file() {
        bail!("thread '{}' has no chat history yet", args.id);
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut items = Vec::new();
    for line in raw.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        if let Ok(item) = serde_json::from_str::<xai_grok_shell::sampling::ConversationItem>(line) {
            items.push(item);
        }
    }
    let start = items.len().saturating_sub(args.limit);
    for item in &items[start..] {
        print_conversation_item(item);
    }
    Ok(())
}

fn print_conversation_item(item: &xai_grok_shell::sampling::ConversationItem) {
    use xai_grok_shell::sampling::{
        AssistantItem, ContentPart, ConversationItem, SystemItem, UserItem,
    };
    match item {
        ConversationItem::System(SystemItem { content }) => {
            println!("system: {}", content.as_ref());
        }
        ConversationItem::User(UserItem { content, .. }) => {
            let text: String = content
                .iter()
                .filter_map(|c| match c {
                    ContentPart::Text { text } => Some(text.as_ref()),
                    _ => None,
                })
                .collect();
            if !text.is_empty() {
                println!("user: {text}");
            }
        }
        ConversationItem::Assistant(AssistantItem { content, .. }) => {
            println!("assistant: {}", content.as_ref());
        }
        _ => {}
    }
}

pub fn send_message(from: &str, to: &str, content: &str) -> Result<()> {
    with_records_mut(|records| {
        if !records.iter().any(|r| r.id == from) {
            bail!("sender thread '{from}' not found");
        }
        let record = records
            .iter_mut()
            .find(|r| r.id == to)
            .ok_or_else(|| anyhow::anyhow!("recipient thread '{to}' not found"))?;
        record.inbox.push(ThreadMessage {
            from: from.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
        });
        record.last_message_at = Utc::now();
        Ok(())
    })?;
    crate::notifications::push(
        "thread_message",
        serde_json::json!({"from": from, "to": to}),
    )?;
    Ok(())
}

fn run_send(args: ThreadSendArgs) -> Result<()> {
    validate_id(&args.from)?;
    validate_id(&args.to)?;
    send_message(&args.from, &args.to, &args.content)?;
    println!("sent message to thread {}", args.to);
    Ok(())
}

fn run_inbox(id: &str) -> Result<()> {
    validate_id(id)?;
    with_records(|records| {
        let record = records
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("thread '{id}' not found"))?;
        if record.inbox.is_empty() {
            println!("no messages for thread {id}");
            return Ok(());
        }
        for m in &record.inbox {
            println!(
                "{} ({})\n  {}",
                m.from,
                m.timestamp.format("%Y-%m-%d %H:%M UTC"),
                m.content
            );
        }
        Ok(())
    })
}

async fn list_models() -> Result<()> {
    let cfg = crate::providers::load_omg_config()?;
    let available = crate::moe::available_providers().await?;
    for id in available {
        let model = cfg
            .providers
            .get(&id)
            .map(|p| p.model.clone())
            .or_else(|| crate::providers::provider_template(&id).map(|t| t.model.clone()))
            .unwrap_or_else(|| "?".to_string());
        println!("omgb-{id} -> {model}");
    }
    if let Some(default) = cfg.default_model {
        println!("default: {default}");
    }
    Ok(())
}

async fn pick_model(task: &str) -> Result<()> {
    let model = pick_model_for_task(task).await?;
    println!("{model}");
    Ok(())
}

async fn pick_model_for_task(task: &str) -> Result<String> {
    let id = crate::moe::select_provider_or_fallback(task).await?;
    Ok(format!("omgb-{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("omgb-threads-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn test_is_safe_id_and_validate_id() {
        assert!(is_safe_id("abc-123_"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id("."));
        assert!(!is_safe_id(".."));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id(" a"));
        assert!(validate_id("ok").is_ok());
        assert!(validate_id("a b").is_err());
        assert!(validate_id("../x").is_err());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));

        let (id, _session_id): (String, String) = with_records_mut(|records| {
            records.push(ThreadRecord {
                id: "t1".into(),
                session_id: "s1".into(),
                cwd: "/tmp".into(),
                model: "omgb-openai".into(),
                created_at: Utc::now(),
                last_message_at: Utc::now(),
                summary: "test thread".into(),
                inbox: vec![],
            });
            Ok(("t1".into(), "s1".into()))
        })
        .unwrap();

        let record: ThreadRecord = with_records(|records| {
            records
                .into_iter()
                .find(|r| r.id == id)
                .ok_or_else(|| anyhow::anyhow!("not found"))
        })
        .unwrap();

        assert_eq!(record.session_id, "s1");

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn test_send_message() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));

        with_records_mut(|records| {
            records.push(ThreadRecord {
                id: "from".into(),
                session_id: "s1".into(),
                cwd: "/tmp".into(),
                model: "omgb-openai".into(),
                created_at: Utc::now(),
                last_message_at: Utc::now(),
                summary: "from".into(),
                inbox: vec![],
            });
            records.push(ThreadRecord {
                id: "to".into(),
                session_id: "s2".into(),
                cwd: "/tmp".into(),
                model: "omgb-openai".into(),
                created_at: Utc::now(),
                last_message_at: Utc::now(),
                summary: "to".into(),
                inbox: vec![],
            });
            Ok(())
        })
        .unwrap();

        send_message("from", "to", "hello").unwrap();

        let inbox = with_records(|records| {
            records
                .into_iter()
                .find(|r| r.id == "to")
                .map(|r| r.inbox)
                .ok_or_else(|| anyhow::anyhow!("to not found"))
        })
        .unwrap();

        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, "from");
        assert_eq!(inbox[0].content, "hello");

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }

    #[test]
    fn test_concurrent_updates() {
        let _g = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = tmp_home();
        std::fs::create_dir_all(&home).unwrap();
        crate::providers::set_omg_home_for_tests(Some(home.clone()));

        let mut handles = Vec::new();
        for i in 0..4 {
            handles.push(std::thread::spawn(move || {
                with_records_mut(|records| {
                    records.push(ThreadRecord {
                        id: format!("t{i}"),
                        session_id: format!("s{i}"),
                        cwd: "/tmp".into(),
                        model: "omgb-openai".into(),
                        created_at: Utc::now(),
                        last_message_at: Utc::now(),
                        summary: format!("thread {i}"),
                        inbox: vec![],
                    });
                    Ok(())
                })
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let records = load_records_unlocked().unwrap();
        assert_eq!(records.len(), 4);

        std::fs::remove_dir_all(&home).ok();
        crate::providers::set_omg_home_for_tests(None);
    }
}
