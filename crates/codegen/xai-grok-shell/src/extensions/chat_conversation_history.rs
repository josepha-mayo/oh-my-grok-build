//! `x.ai/session/page`: fetch one older page of a gateway-backed
//! conversation by client-owned cursor (`beforeId` -> `nextBeforeId`).
use super::{ExtResult, parse_params, to_ext_response};
use crate::agent::MvpAgent;
use agent_client_protocol as acp;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const MAX_PAGE_LIMIT: usize = 100;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageRequest {
    session_id: String,
    #[serde(default)]
    before_id: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    50
}

#[derive(Debug, Serialize, PartialEq, Clone)]
#[serde(rename_all = "camelCase")]
struct PageResponse {
    messages: Vec<PageMessage>,
    next_before_id: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
#[serde(rename_all = "camelCase")]
struct PageMessage {
    id: String,
    role: String,
    text: String,
}

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let req: PageRequest = parse_params(args)?;
    let session_id = acp::SessionId::new(req.session_id);

    let handle = {
        let sessions = agent.sessions.borrow();
        sessions.get(&session_id).cloned()
    };

    let Some(handle) = handle else {
        return to_ext_response(Ok(PageResponse {
            messages: Vec::new(),
            next_before_id: None,
        }));
    };

    let conversation = handle.chat_state_handle.get_conversation().await;
    let messages: Vec<PageMessage> = conversation
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| to_page_message(idx, item))
        .collect();

    let result = paginate(&messages, req.before_id.as_deref(), req.limit).map(
        |(messages, next_before_id)| PageResponse {
            messages,
            next_before_id,
        },
    );
    to_ext_response(result)
}

fn paginate(
    messages: &[PageMessage],
    before_id: Option<&str>,
    limit: usize,
) -> Result<(Vec<PageMessage>, Option<String>)> {
    let total = messages.len();
    let end = match before_id {
        Some(id) => id
            .parse::<usize>()
            .with_context(|| format!("invalid before_id: {id}"))?,
        None => total,
    };

    // Out-of-range cursors are clamped to the newest message so a stale/compact
    // conversation does not return an empty page.
    let end = end.min(total);
    let limit = limit.clamp(1, MAX_PAGE_LIMIT);
    let start = end.saturating_sub(limit);
    let page = messages[start..end].to_vec();
    let next_before_id = if start > 0 {
        Some(start.to_string())
    } else {
        None
    };
    Ok((page, next_before_id))
}

fn to_page_message(
    idx: usize,
    item: &xai_grok_sampling_types::ConversationItem,
) -> Option<PageMessage> {
    use xai_grok_sampling_types::{ContentPart, ConversationItem};

    let (role, text) = match item {
        ConversationItem::User(u) => {
            let text = u
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            ("user", text)
        }
        ConversationItem::Assistant(a) => ("assistant", a.content.to_string()),
        ConversationItem::ToolResult(t) => ("tool", t.content.to_string()),
        _ => return None,
    };

    if text.is_empty() {
        return None;
    }

    Some(PageMessage {
        id: idx.to_string(),
        role: role.to_string(),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_messages(count: usize) -> Vec<PageMessage> {
        (0..count)
            .map(|i| PageMessage {
                id: i.to_string(),
                role: "user".to_string(),
                text: format!("msg {i}"),
            })
            .collect()
    }

    #[test]
    fn paginate_no_cursor_returns_last_page() {
        let msgs = sample_messages(10);
        let (page, next) = paginate(&msgs, None, 3).unwrap();
        assert_eq!(page.len(), 3);
        assert_eq!(page[0].id, "7");
        assert_eq!(next, Some("7".to_string()));
    }

    #[test]
    fn paginate_cursor_steps_backwards() {
        let msgs = sample_messages(10);
        let (page, next) = paginate(&msgs, Some("7"), 3).unwrap();
        assert_eq!(page.len(), 3);
        assert_eq!(page[0].id, "4");
        assert_eq!(next, Some("4".to_string()));
    }

    #[test]
    fn paginate_out_of_range_cursor_clamps() {
        let msgs = sample_messages(5);
        let (page, next) = paginate(&msgs, Some("100"), 2).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].id, "3");
        assert_eq!(next, Some("3".to_string()));
    }

    #[test]
    fn paginate_invalid_cursor_errors() {
        let msgs = sample_messages(5);
        assert!(paginate(&msgs, Some("abc"), 2).is_err());
    }

    #[test]
    fn paginate_limit_clamped() {
        let msgs = sample_messages(5);
        let (page, _) = paginate(&msgs, None, 0).unwrap();
        assert_eq!(page.len(), 1);
        let (page, _) = paginate(&msgs, None, 1000).unwrap();
        assert_eq!(page.len(), 5);
    }

    #[test]
    fn paginate_start_of_conversation_has_no_next_cursor() {
        let msgs = sample_messages(2);
        let (page, next) = paginate(&msgs, Some("2"), 2).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(next, None);
    }
}
