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
//!
//! # The reply carries the progress (#2109)
//!
//! A guest summon gets one reply, and the bot is not in the chat, so there is
//! no typing indicator and no second bubble to post. The reply itself is the
//! only surface, which is exactly the surface Telegram designed: its Guest
//! Bots page lists "AI Assistants ... reporting its progress back to the chat
//! or group" as the first use case.
//!
//! So the reply is posted the moment the turn starts (a plain
//! [`GUEST_PLACEHOLDER`] line, since the initial `answerGuestQuery` body
//! carries no parse mode) and then edited in place: `answerGuestQuery` returns
//! `SentGuestMessage.inline_message_id`, and `editMessageText` accepts
//! `inline_message_id` (required when `chat_id`/`message_id` are absent) and
//! returns `True`. Edits ride the same 1.5s cadence the regular chat's edit
//! loop uses, and the state being rendered is the regular
//! [`StreamingState`] fed by the regular
//! [`apply_progress_event`](super::progress::apply_progress_event) mapping, so
//! the block, the tool lines, the counters and the settled footer are the same
//! code as a normal session rather than a guest-shaped copy.
//!
//! Two things this deliberately does NOT copy from the regular path: the
//! typing indicator (the bot is not a member, so there is nothing to type in)
//! and the per-tool messages (one reply is all the API gives).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::flow::{
    FlowEntry, FlowOutcome, StreamingState, drain_display_queue, folded_duplicates_final,
    humanize_duration, last_folded_text, pop_trailing_folded_texts, render_flow,
};
use super::handler::thinking_status_excerpt;
use super::markdown::markdown_to_telegram_html;
use super::progress::apply_progress_event;
use crate::brain::agent::AgentService;
use crate::config::Config;
use crate::services::SessionService;

/// Telegram's ceiling for `InputTextMessageContent.message_text`.
const GUEST_TEXT_LIMIT: usize = 4096;
/// Inline result id, kept well under Telegram's 64-byte cap.
const GUEST_RESULT_ID: &str = "opencrabs-guest";
/// Appended when a reply has to be cut to fit the message ceiling.
const TRUNCATION_MARKER: &str = "\n\n[truncated]";
/// What the reply says between being posted and the first real edit. Plain
/// text on purpose: the opening `answerGuestQuery` carries no parse mode.
const GUEST_PLACEHOLDER: &str = "⏳ Working on it…";
/// How often the open reply is edited while the turn runs. Same cadence as
/// the regular chat's edit loop.
const GUEST_EDIT_INTERVAL: Duration = Duration::from_millis(1500);
/// Below this much room left for the answer, the block is not worth showing:
/// a footer with three words under it helps nobody, so the answer ships alone.
const MIN_ANSWER_ROOM: usize = 200;

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

/// Length in the units Telegram counts: UTF-16 code units after entities are
/// parsed, so an emoji counts 2. `chars().count()` under-counts astral text
/// and lets an over-long message through to a "message is too long" error.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Characters Telegram will actually show, i.e. everything outside `<...>`
/// tags. Conservative where it is approximate: `&amp;` counts 5, not 1, so the
/// budget it produces is always small enough to fit.
pub(crate) fn visible_len(html: &str) -> usize {
    let mut count = 0usize;
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => count += ch.len_utf16(),
            _ => {}
        }
    }
    count
}

/// Trim text to `room` UTF-16 code units, marker included.
fn fit_text(text: &str, room: usize) -> String {
    if utf16_len(text) <= room {
        return text.to_string();
    }
    let marker = utf16_len(TRUNCATION_MARKER);
    let keep = room.saturating_sub(marker);
    let mut used = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let width = ch.len_utf16();
        if used + width > keep {
            break;
        }
        out.push(ch);
        used += width;
    }
    if room >= marker {
        out.push_str(TRUNCATION_MARKER);
    }
    out
}

/// Trim a reply down to what a guest message may carry.
fn clamp_guest_text(text: &str) -> String {
    fit_text(text, GUEST_TEXT_LIMIT)
}

/// The one message a guest reply can be: the turn's block (tool lines plus the
/// settled footer) with the answer under it.
///
/// Always inside [`GUEST_TEXT_LIMIT`]. When the block leaves too little room
/// for a real answer the answer ships alone, because the block is chrome and
/// the answer is the point.
pub(crate) fn compose_guest_message(block_html: &str, answer_markdown: &str) -> String {
    let answer = clamp_guest_text(answer_markdown.trim());
    let room = GUEST_TEXT_LIMIT.saturating_sub(visible_len(block_html) + 2);
    if block_html.is_empty() || room < MIN_ANSWER_ROOM {
        return answer;
    }
    let fitted = fit_text(answer_markdown.trim(), room);
    let composed = if fitted.is_empty() {
        block_html.to_string()
    } else {
        format!("{block_html}\n\n{}", markdown_to_telegram_html(&fitted))
    };
    if visible_len(&composed) > GUEST_TEXT_LIMIT {
        return answer;
    }
    composed
}

/// Take the answer out of the flow log so the settled reply carries it once.
///
/// The streamed answer also reaches the flow log as intermediate text, so a
/// settled block renders it and the reply renders it again: the caller sees
/// the same paragraph twice in one message (#2113). The regular delivery path
/// reclaims that copy before it sends the answer (#300); a guest reply is a
/// single message, so the same reclaim has to happen here.
///
/// - No content from the turn: the folded run IS the answer and comes back as
///   the reply. With `options_pending` the answer sits before the trailing
///   tool entry, so the popper lifts that entry aside and restores it.
/// - Content that duplicates the folded run: the folded copy leaves the block.
/// - A folded run that is neither stays where it is, because a narration line
///   that merely ended the turn is not the answer.
pub(crate) fn settle_guest_answer(
    entries: &mut Vec<FlowEntry>,
    content: &str,
    options_pending: bool,
) -> String {
    if content.trim().is_empty() {
        let (host, trailer) = pop_trailing_folded_texts(entries, options_pending);
        return match (host, trailer) {
            (Some(host), Some(trailer)) => format!("{host}\n\n{trailer}"),
            (Some(host), None) => host,
            (None, _) => String::new(),
        };
    }
    // The reply carries the answer, so any copy of it left in the block would
    // be read twice. Drop only the runs that duplicate it: a distinct narration
    // line that merely ended the turn is not the answer and stays where it is.
    while last_folded_text(entries).is_some_and(|folded| folded_duplicates_final(folded, content)) {
        let Some(pos) = entries
            .iter()
            .rposition(|e| matches!(e, FlowEntry::Text(_)))
        else {
            break;
        };
        entries.remove(pos);
    }
    // A turn that halted on the options surface keeps its answer BEFORE the
    // trailing tool entry, where `last_folded_text` refuses to look. The shared
    // popper is the only thing that knows how to lift that entry aside and put
    // it back, so it runs on a copy and is kept only when the run it reclaimed
    // really is the answer.
    if options_pending {
        let mut trial = entries.clone();
        let (host, trailer) = pop_trailing_folded_texts(&mut trial, true);
        let reclaimed = [host, trailer]
            .into_iter()
            .flatten()
            .any(|run| folded_duplicates_final(&run, content));
        if reclaimed {
            *entries = trial;
        }
    }
    content.to_string()
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

/// A reply as it goes on the wire: directives stripped, whitespace trimmed,
/// secrets scrubbed.
///
/// The scrub is not optional here. `redact_secrets_scoped(text, true)` would
/// let credentials through, which is right for the owner's own DM and wrong
/// for a stranger's group, so a guest reply always takes the scrubbing branch.
pub(crate) fn prepare_guest_text(reply: &str) -> String {
    let text = crate::utils::sanitize::strip_llm_artifacts(reply);
    let stripped = strip_directives(&text).trim().to_string();
    crate::utils::redact_secrets_scoped(&stripped, false)
}

/// Deny every approval. See the module docs: a guest turn has no chat to put
/// an approval button in, and the caller is not the machine's owner.
fn deny_all_approvals() -> crate::brain::agent::ApprovalCallback {
    Arc::new(|_info| Box::pin(async { Ok((false, false)) }))
}

/// Fold progress events into the turn's state.
///
/// The same mapping a regular Telegram turn uses; only the surface that
/// renders the state differs (a chat message here, an edited guest reply
/// there).
fn guest_progress_cb(state: &Arc<Mutex<StreamingState>>) -> crate::brain::agent::ProgressCallback {
    let st = Arc::clone(state);
    Arc::new(move |_session_id, event| apply_progress_event(&st, event))
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

    async fn answer(self: &Arc<Self>, query: GuestQuery) {
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
        // Never a DM: a guest reply lands in a chat we do not control, so the
        // state is built in the scrubbing scope and both surfaces (the block
        // and the answer) are redacted before they reach the wire.
        let state = Arc::new(Mutex::new(StreamingState::new_turn(
            false,
            self.agent
                .provider_for_session(session_id)
                .cli_handles_tools(),
        )));
        let cancel = CancellationToken::new();

        // Open the reply before the turn runs. Guest mode gives the bot one
        // reply per summon and nothing else, so the message has to exist
        // first: an edit can only change a message that is already there, and
        // posting it up front is what turns that single reply into a live
        // progress surface (#2109). A failure here is not fatal: the turn
        // still runs and the answer is posted in one piece at the end.
        let opened = match self
            .post_answer(&answer_guest_query_body(&query.query_id, GUEST_PLACEHOLDER))
            .await
        {
            Ok(inline_message_id) => Some(inline_message_id),
            Err(e) => {
                tracing::warn!(
                    "Telegram guest: could not open the reply for query {} (the answer will be \
                     posted in one piece at the end): {e}",
                    query.query_id
                );
                None
            }
        };
        let edit_target = opened.as_ref().filter(|id| !id.is_empty()).cloned();

        let edit_handle = edit_target.as_ref().map(|inline_message_id| {
            tokio::spawn(edit_tick_loop(
                Arc::clone(self),
                Arc::clone(&state),
                inline_message_id.clone(),
                cancel.clone(),
            ))
        });

        let input = format!(
            "[Channel: Telegram guest query: you were tagged in \"{}\" (chat {}), a chat you \
             are NOT a member of. Your text response is posted there, and while the turn runs \
             the caller sees a status block listing the tools you use; that block is edited in \
             place, so do not narrate your own progress, just do the work. The caller sees no \
             tool output and has no access to that chat's history, so keep the answer short and \
             self-contained. Do NOT call telegram_send; this conversation has no chat id.]\n\n{}",
            query.chat_title.as_deref().unwrap_or("a chat"),
            chat_key.as_deref().unwrap_or("unknown"),
            query.text,
        );
        let response = self
            .agent
            .send_message_with_tools_and_display(
                session_id,
                input,
                Some(query.text.clone()),
                None,
                Some(cancel.clone()),
                Some(deny_all_approvals()),
                Some(guest_progress_cb(&state)),
                "telegram",
                None,
                None,
            )
            .await;

        // The turn is over: stop ticking before the terminal render, so a live
        // edit can never land after the settled one.
        cancel.cancel();
        if let Some(handle) = edit_handle {
            let _ = handle.await;
        }

        let response = match response {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!(
                    "Telegram guest: turn failed for query {} (session {}): {e}",
                    query.query_id,
                    session_id
                );
                // The reply is already in the chat: settle it as failed rather
                // than leaving a stranger watching a live footer.
                if let Some(inline_message_id) = edit_target.as_deref() {
                    let block = settle_block(&state, FlowOutcome::Failed, None);
                    if let Err(e) = self.edit_answer(inline_message_id, &block).await {
                        tracing::debug!(
                            "Telegram guest: could not settle the failed reply for query {}: {e}",
                            query.query_id
                        );
                    }
                }
                return;
            }
        };

        // The answer also streamed into the flow log as intermediate text, so
        // rendering the settled block as-is would carry it twice: once inside
        // the block and once under it (#2113). Reclaim it the way the regular
        // delivery path does (#300) before the block is rendered.
        let content = prepare_guest_text(&response.content);
        let reply = {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            let options_pending = s.pending_suggestions.is_some();
            settle_guest_answer(&mut s.flow_entries, &content, options_pending)
        };
        if reply.is_empty() {
            tracing::warn!(
                "Telegram guest: query {} produced an empty reply, sending the block alone",
                query.query_id
            );
        }
        // Settle the block BEFORE composing it with the answer, so the reply
        // carries the footer a regular turn ends on: outcome, tool count,
        // context budget, clock.
        let ctx_max = self.agent.context_limit_for_session(session_id);
        let ctx = crate::utils::format_ctx_footer(
            response.context_tokens,
            ctx_max,
            response.tokens_per_second,
        );
        let block = settle_block(&state, FlowOutcome::Finished, Some(ctx));
        let message = compose_guest_message(&block, &reply);
        self.deliver(
            &query.query_id,
            opened.as_deref(),
            edit_target.as_deref(),
            &message,
            &reply,
        )
        .await;
    }

    /// Put the finished message in the chat.
    ///
    /// The normal path edits the reply that was opened before the turn. The
    /// fallback posts it in one piece, which is what happens when opening
    /// failed earlier (so the turn still answers) or when the final edit was
    /// refused. When the reply is open but the API never gave an id for it,
    /// there is nothing to edit and nothing to post, so the turn ends there
    /// rather than answering twice.
    async fn deliver(
        &self,
        query_id: &str,
        opened: Option<&str>,
        edit_target: Option<&str>,
        message: &str,
        plain: &str,
    ) {
        match edit_target {
            Some(inline_message_id) => match self.edit_answer(inline_message_id, message).await {
                Ok(()) => tracing::info!(
                    "Telegram guest: answered query {query_id} by editing the open reply"
                ),
                Err(e) => {
                    tracing::warn!(
                        "Telegram guest: final edit failed for query {query_id} ({e}); posting \
                         the answer in one piece instead"
                    );
                    self.post_once(query_id, plain).await;
                }
            },
            None if opened.is_none() => self.post_once(query_id, plain).await,
            None => tracing::warn!(
                "Telegram guest: query {query_id} has an open reply but no inline_message_id \
                 to edit it by; leaving the reply as posted"
            ),
        }
    }

    /// Post the answer as its own message. Used when the reply could not be
    /// opened, and after a refused final edit.
    async fn post_once(&self, query_id: &str, plain: &str) {
        if plain.is_empty() {
            tracing::warn!("Telegram guest: query {query_id} has no text to post");
            return;
        }
        match self
            .post_answer(&answer_guest_query_body(query_id, plain))
            .await
        {
            Ok(_) => tracing::info!("Telegram guest: answered query {query_id} in one piece"),
            Err(e) => {
                tracing::warn!("Telegram guest: answerGuestQuery failed for query {query_id}: {e}")
            }
        }
    }

    /// POST `answerGuestQuery` through the raw HTTP layer and return the
    /// `inline_message_id` of the message it placed. That id is the handle
    /// every later edit needs, and it is the reason the reply is posted before
    /// the turn instead of after it.
    ///
    /// Neither the method nor `SentGuestMessage` has a typed binding in
    /// teloxide-core 0.13.0, same as the rest of the guest path.
    async fn post_answer(&self, body: &Value) -> Result<String, String> {
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
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if status.is_success() && ok {
            // Absent id is not an error: the reply is in the chat, it just
            // cannot be edited. An empty id says exactly that to the caller.
            return Ok(value
                .get("result")
                .and_then(|r| r.get("inline_message_id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string());
        }
        Err(format!(
            "status {status}: {}",
            crate::utils::truncate_str(&text, 300)
        ))
    }

    /// Edit the open reply in place. The twin of the regular path's
    /// `edit_rich_html` / HTML edit, one API method over.
    async fn edit_answer(&self, inline_message_id: &str, html: &str) -> Result<(), String> {
        let url = format!("https://api.telegram.org/bot{}/editMessageText", self.token);
        let body = json!({
            "inline_message_id": inline_message_id,
            "text": html,
            "parse_mode": "HTML",
        });
        let resp = reqwest::Client::new()
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if edit_response_ok(&value) {
            return Ok(());
        }
        Err(format!(
            "status {status}: {}",
            crate::utils::truncate_str(&text, 300)
        ))
    }
}

/// Whether an `editMessageText` response means the edit landed.
///
/// Writing identical text is not a failure: nothing moved, which is what the
/// caller wanted. Telegram still reports it as an error, so it is read here
/// and treated as success. A false positive needs a response that is not 200
/// and not that exact error string, which the API does not send.
pub(crate) fn edit_response_ok(body: &Value) -> bool {
    if body.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return true;
    }
    body.get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("message is not modified")
}

/// The stripping the regular edit loop runs before a line reaches the log:
/// artifacts off first, then secrets scrubbed, then the channel markers out.
/// The split matters: chrome is our own text, so it only needs the scrub,
/// while model narration also carries artifacts and markers.
///
/// A guest reply lands in a chat we do not control, so every pass runs in the
/// scrubbing scope and an `<<IMG:/Users/...>>` can never show up there as a
/// local path.
pub(crate) fn guest_sanitize(text: &str, system: bool) -> String {
    if system {
        return crate::utils::redact_secrets_scoped(text, false);
    }
    let text = crate::utils::sanitize::strip_llm_artifacts(text);
    let text = crate::utils::redact_secrets_scoped(&text, false);
    let (text, _images) = crate::utils::extract_img_markers(&text);
    let (text, _reaction) = crate::utils::extract_react_marker(&text);
    text
}

/// Render the live reply: drain the queue, advance the clock and the status
/// line, then render exactly what a regular chat renders.
fn pump(state: &Arc<Mutex<StreamingState>>) -> String {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    drain_display_queue(&mut s, guest_sanitize);
    let elapsed = s.turn_started_at.elapsed().as_secs();
    // Mirrors `tick_flow_header`: no duration at 0s, then the humanized one.
    s.flow_status = (elapsed > 0).then(|| humanize_duration(elapsed));
    s.header_preview = thinking_status_excerpt(&s.thinking);
    render_flow(&s)
}

/// Settle the turn's block and render it. `ctx` is display-only chrome set at
/// settle time, matching the regular delivery path.
fn settle_block(
    state: &Arc<Mutex<StreamingState>>,
    outcome: FlowOutcome,
    ctx: Option<String>,
) -> String {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    drain_display_queue(&mut s, guest_sanitize);
    s.flow_outcome = Some(outcome);
    s.tools_started_at = None;
    if ctx.is_some() {
        s.sections.ctx = ctx;
    }
    render_flow(&s)
}

/// Edit the open reply on the regular cadence until the turn ends.
async fn edit_tick_loop(
    me: Arc<GuestRunner>,
    state: Arc<Mutex<StreamingState>>,
    inline_message_id: String,
    cancel: CancellationToken,
) {
    let mut last = String::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(GUEST_EDIT_INTERVAL) => {}
        }
        let html = pump(&state);
        if html.is_empty() || html == last {
            continue;
        }
        match me.edit_answer(&inline_message_id, &html).await {
            Ok(()) => last = html,
            // A live edit is cosmetic: a rate limit or a dropped socket must
            // not end the turn, so it is logged and the next tick renders the
            // newer state anyway.
            Err(e) => tracing::debug!("Telegram guest: live edit skipped: {e}"),
        }
    }
}
