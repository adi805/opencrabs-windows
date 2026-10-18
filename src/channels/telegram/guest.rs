//! Guest mode (#2087): answer `@botname` tags in chats the bot is not in.
//!
//! Bot API 10.0 ("Guest Mode", May 8 2026) added `Update.guest_message`, the
//! fields `Message.guest_query_id`, `Message.guest_bot_caller_user` and
//! `Message.guest_bot_caller_chat`, and the method `answerGuestQuery`
//! (verified against core.telegram.org/bots/api on 2026-10-18). The pinned
//! teloxide-core 0.13.0 knows none of it: `grep -rni guest` over its source
//! returns nothing, so a guest update parses into `UpdateKind::Error` and no
//! dispatch branch matches it. The whole path therefore lives on the raw HTTP
//! layer that already polls `getUpdates` (`raw_updates.rs`), exactly like
//! `message_reaction` did before teloxide grew a typed variant for it.
//!
//! Default deny, in two independent places. A guest reply answers a stranger
//! in a chat we were never invited to, so it needs `guest_queries = true` in
//! config AND the caller to be the owner, an admin, or listed for the caller's
//! own chat. A group's `open = true` grants nothing here: it records that the
//! owner opened THAT chat for the bot, not that the bot answers anywhere that
//! group's members happen to tag it.
//!
//! Tools that need approval are denied for a guest turn. The approval surface
//! is buttons in a chat, and a guest turn has no chat to put them in; denying
//! is the only safe default when the caller does not own the machine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::brain::agent::AgentService;
use crate::config::Config;
use crate::services::SessionService;

/// Telegram's ceiling for `InputTextMessageContent.message_text`.
const GUEST_TEXT_LIMIT: usize = 4096;
/// Inline result id, kept well under Telegram's 64-byte cap.
const GUEST_RESULT_ID: &str = "opencrabs-guest";
/// Appended when a reply has to be cut to fit the message ceiling.
const TRUNCATION_MARKER: &str = "\n\n[truncated]";

/// One parsed guest query: who summoned the bot, in which chat, and what they
/// said. Text only: a guest turn never sees the surrounding chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuestQuery {
    /// `Message.guest_query_id`, the value `answerGuestQuery` needs.
    pub query_id: String,
    /// `Message.guest_bot_caller_user.id`: who tagged the bot.
    pub user_id: i64,
    pub username: Option<String>,
    /// `Message.guest_bot_caller_chat.id`: the chat the tag came from. This id
    /// may collide with a chat the bot is already in, which is why guest
    /// sessions never bind to it in the shared chat map.
    pub chat_id: Option<i64>,
    pub chat_title: Option<String>,
    pub text: String,
}

/// Parse `Update.guest_message` out of a raw update payload.
///
/// `None` means "not a guest query we can answer": not a guest update at all,
/// no `guest_query_id` to answer into, or no identifiable caller (in which
/// case the gate has nothing to check and the query is dropped rather than
/// answered on faith).
pub(crate) fn parse_guest_query(update: &Value) -> Option<GuestQuery> {
    let msg = update.get("guest_message")?;
    let query_id = msg
        .get("guest_query_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if query_id.is_empty() {
        return None;
    }
    let caller_user = msg.get("guest_bot_caller_user").or_else(|| msg.get("from"));
    let user_id = caller_user
        .and_then(|u| u.get("id"))
        .and_then(Value::as_i64)?;
    let username = caller_user
        .and_then(|u| u.get("username"))
        .and_then(Value::as_str)
        .map(str::to_string);
    // The API says the tagged message belongs to the caller's chat; fall back
    // to the message's own `chat` when `guest_bot_caller_chat` is absent, so a
    // listed user still passes the per-chat check.
    let caller_chat = msg.get("guest_bot_caller_chat").or_else(|| msg.get("chat"));
    let chat_id = caller_chat
        .and_then(|c| c.get("id"))
        .and_then(Value::as_i64);
    let chat_title = caller_chat
        .and_then(|c| c.get("title"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let text = msg
        .get("text")
        .or_else(|| msg.get("caption"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(GuestQuery {
        query_id: query_id.to_string(),
        user_id,
        username,
        chat_id,
        chat_title,
        text,
    })
}

/// Drop `<<...>>` directives (reactions, image markers) from a guest reply.
///
/// Those directives are channel instructions, and this is the one surface
/// where the raw text leaves the process into a stranger's chat: an
/// un-processed `<<IMG:/Users/...>>` would post a local file path there.
pub(crate) fn strip_directives(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<<") {
        let after = &rest[start + 2..];
        match after.find(">>") {
            Some(end) if !after[..end].contains('\n') => {
                out.push_str(&rest[..start]);
                rest = &after[end + 2..];
            }
            _ => {
                out.push_str(&rest[..start + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Trim a reply down to what a guest message may carry.
fn clamp_guest_text(text: &str) -> String {
    if text.chars().count() <= GUEST_TEXT_LIMIT {
        return text.to_string();
    }
    let keep = GUEST_TEXT_LIMIT - TRUNCATION_MARKER.chars().count();
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(TRUNCATION_MARKER);
    out
}

/// Short single-line title for the inline article. Telegram shows it in the
/// client while the message is being sent, so it must never be the full reply.
fn guest_title(text: &str) -> String {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .unwrap_or("Reply");
    let mut title: String = first.chars().take(64).collect();
    if title.is_empty() {
        title.push_str("Reply");
    }
    title
}

/// The `answerGuestQuery` request body: the query id plus one
/// `InlineQueryResultArticle` carrying the reply as its message content.
///
/// No `parse_mode` is set, so the reply arrives as plain text. Guest replies
/// are not routed through the rich-message or HTML paths: those exist to shape
/// a bot message the client renders, and this is one text payload handed to
/// the API.
pub(crate) fn answer_guest_query_body(query_id: &str, text: &str) -> Value {
    json!({
        "guest_query_id": query_id,
        "result": {
            "type": "article",
            "id": GUEST_RESULT_ID,
            "title": guest_title(text),
            "input_message_content": { "message_text": clamp_guest_text(text) },
        }
    })
}

/// A reply as it goes on the wire: directives stripped, whitespace trimmed.
pub(crate) fn prepare_guest_text(reply: &str) -> String {
    strip_directives(reply).trim().to_string()
}

/// Deny every approval. See the module docs: a guest turn has no chat to put
/// an approval button in, and the caller is not the machine's owner.
fn deny_all_approvals() -> crate::brain::agent::ApprovalCallback {
    Arc::new(|_info| Box::pin(async { Ok((false, false)) }))
}

/// Swallow progress events: nothing about a guest turn may be posted anywhere
/// except the one answer, and there is no chat to post it to.
fn silent_progress() -> crate::brain::agent::ProgressCallback {
    Arc::new(|_session_id, _event| {})
}

/// Runs guest turns: gates them, drives one agent turn, answers the query.
pub(crate) struct GuestRunner {
    token: Arc<String>,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    config_rx: tokio::sync::watch::Receiver<Config>,
    /// One session per (caller, caller chat). In memory only: guest content
    /// never enters the group history store, and a restart giving the same
    /// caller a fresh session costs one turn of context, not correctness.
    sessions: Mutex<HashMap<(i64, i64), Uuid>>,
}

impl GuestRunner {
    pub(crate) fn new(
        token: Arc<String>,
        agent: Arc<AgentService>,
        session_svc: SessionService,
        config_rx: tokio::sync::watch::Receiver<Config>,
    ) -> Arc<Self> {
        Arc::new(Self {
            token,
            agent,
            session_svc,
            config_rx,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Hand a guest query to a background task. The poll loop must never wait
    /// on a guest turn: it is a full agent turn and can run for minutes.
    pub(crate) fn spawn(self: &Arc<Self>, query: GuestQuery) {
        let me = Arc::clone(self);
        tokio::spawn(async move { me.answer(query).await });
    }

    /// Session for this caller pair, created on first use.
    async fn session_for(&self, query: &GuestQuery) -> Uuid {
        let key = (query.user_id, query.chat_id.unwrap_or(i64::MIN));
        if let Some(id) = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .copied()
        {
            return id;
        }
        let title = match (&query.chat_title, query.chat_id) {
            (Some(t), Some(c)) => format!("Guest: {t} ({c})"),
            (Some(t), None) => format!("Guest: {t}"),
            (None, Some(c)) => format!("Guest chat {c}"),
            (None, None) => "Guest chat".to_string(),
        };
        let session_id = match crate::channels::session_init::create_channel_session(
            &self.session_svc,
            Some(title),
            None,
        )
        .await
        {
            Ok(s) => s.id,
            Err(e) => {
                // Not cached: the next query retries rather than binding
                // this pair to a session that does not exist.
                tracing::warn!("Telegram guest: session create failed: {e}");
                return Uuid::new_v4();
            }
        };
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, session_id);
        session_id
    }

    async fn answer(&self, query: GuestQuery) {
        let cfg = self.config_rx.borrow().clone();
        let chat_key = query.chat_id.map(|c| c.to_string());
        if !cfg
            .channels
            .telegram
            .guest_allowed(&query.user_id.to_string(), chat_key.as_deref())
        {
            tracing::info!(
                "Telegram guest: refusing query {} from user {} ({}) in chat {:?} \
                 (guest_queries={}); owner, admin, or a listing for that chat is required",
                query.query_id,
                query.user_id,
                query.username.as_deref().unwrap_or("unknown"),
                query.chat_id,
                cfg.channels.telegram.guest_queries,
            );
            return;
        }
        if query.text.trim().is_empty() {
            tracing::info!(
                "Telegram guest: query {} from user {} carries no text, nothing to answer \
                 (guest turns are text only)",
                query.query_id,
                query.user_id
            );
            return;
        }
        tracing::info!(
            "Telegram guest: answering query {} from user {} ({}) in chat {:?} (\"{}\")",
            query.query_id,
            query.user_id,
            query.username.as_deref().unwrap_or("unknown"),
            query.chat_id,
            query.chat_title.as_deref().unwrap_or("unknown"),
        );
        let session_id = self.session_for(&query).await;
        let input = format!(
            "[Channel: Telegram guest query: you were tagged in \"{}\" (chat {}), a chat you \
             are NOT a member of. Your text response is posted there as a single message and is \
             the only thing the caller sees: no tool output, no intermediates, and no access to \
             that chat's history. Do NOT call telegram_send; this conversation has no chat id. \
             Keep the answer short and self-contained.]\n\n{}",
            query.chat_title.as_deref().unwrap_or("a chat"),
            chat_key.as_deref().unwrap_or("unknown"),
            query.text,
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let reply = match self
            .agent
            .send_message_with_tools_and_display(
                session_id,
                input,
                Some(query.text.clone()),
                None,
                Some(cancel),
                Some(deny_all_approvals()),
                Some(silent_progress()),
                "telegram",
                None,
                None,
            )
            .await
        {
            Ok(response) => prepare_guest_text(&response.content),
            Err(e) => {
                tracing::warn!(
                    "Telegram guest: turn failed for query {} (session {}): {e}",
                    query.query_id,
                    session_id
                );
                return;
            }
        };
        if reply.is_empty() {
            tracing::warn!(
                "Telegram guest: query {} produced an empty reply, nothing sent",
                query.query_id
            );
            return;
        }
        let body = answer_guest_query_body(&query.query_id, &reply);
        match self.post_answer(&body).await {
            Ok(()) => tracing::info!(
                "Telegram guest: answered query {} to chat {:?}",
                query.query_id,
                query.chat_id
            ),
            Err(e) => tracing::warn!(
                "Telegram guest: answerGuestQuery failed for query {}: {e}",
                query.query_id
            ),
        }
    }

    /// POST the answer through the raw HTTP layer. `answerGuestQuery` has no
    /// typed binding in teloxide-core 0.13.0, same as the rest of the guest
    /// path.
    async fn post_answer(&self, body: &Value) -> Result<(), String> {
        let url = format!(
            "https://api.telegram.org/bot{}/answerGuestQuery",
            self.token
        );
        let resp = reqwest::Client::new()
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let ok = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("ok").and_then(Value::as_bool))
            .unwrap_or(false);
        if status.is_success() && ok {
            return Ok(());
        }
        Err(format!(
            "status {status}: {}",
            crate::utils::truncate_str(&text, 300)
        ))
    }
}
