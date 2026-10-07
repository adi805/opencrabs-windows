//! Ephemeral group replies: Bot API 10.3 (`ephemeral_message_parameters`),
//! 2026-08-24; the pre-10.3 flat `receiver_user_id` from 10.2 (2026-07-14) is
//! kept as a one-shot runtime fallback.
//!
//! Telegram can deliver a group message to a single member: nobody else in
//! the chat ever sees it. Every OpenCrabs slash command is owner-gated, so
//! today the whole group watches replies addressed to one person. Scoping
//! those to the invoker keeps the group context and drops the noise (#756).
//!
//! teloxide 0.17 / teloxide-core 0.13 has no binding for the parameter, so
//! this calls `sendMessage` directly over HTTP, mirroring [`super::rich::api`].
//! Whether `sendRichMessage` also accepts the parameter is settled at runtime
//! rather than assumed: see [`try_send_rich`]. Scoped replies fall back to
//! HTML only once the server has actually refused the rich variant.
//!
//! Nothing here returns an error. A send that does not land reports how much
//! was delivered and the caller finishes the job on its normal public path,
//! which is exactly the pre-10.2 behaviour. That covers both a Bot API server
//! older than 10.2 and a future rename of the parameter.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use teloxide::Bot;
use teloxide::types::{ChatId, ThreadId};

/// The ephemeral picker currently on screen per chat, newest last.
///
/// A scoped message carries `message_id: 0`, so the callback that follows a
/// tap on its keyboard says only "message 0 in this chat": the id the
/// `editEphemeralMessage*` and `deleteEphemeralMessage` methods need exists
/// nowhere else. It is remembered here rather than in [`super::state`] because
/// the callback UI path (`edit_retry::edit_text_ui`) is a plain function with
/// no `TelegramState` in hand, and this is the same shape as the raw-message
/// stash next door: a small bounded process-local map.
///
/// Bounded like `RAW_STASH`: a chat that never taps its picker must not grow
/// this without limit.
static EPHEMERAL_PICKERS: Mutex<VecDeque<(i64, i64)>> = Mutex::new(VecDeque::new());
const PICKER_CAP: usize = 64;

/// Remember the ephemeral picker sent to `chat_id`, returning the id of the
/// one it replaced so the caller can delete that stale bubble.
pub(crate) fn remember_picker(chat_id: i64, ephemeral_message_id: i64) -> Option<i64> {
    let mut q = EPHEMERAL_PICKERS.lock().unwrap_or_else(|e| e.into_inner());
    let replaced = q
        .iter()
        .find(|(c, _)| *c == chat_id)
        .map(|(_, id)| *id)
        .filter(|id| *id != ephemeral_message_id);
    q.retain(|(c, _)| *c != chat_id);
    q.push_back((chat_id, ephemeral_message_id));
    while q.len() > PICKER_CAP {
        q.pop_front();
    }
    replaced
}

/// The ephemeral picker id for a chat, if one is on screen.
pub(crate) fn picker_for(chat_id: i64) -> Option<i64> {
    let q = EPHEMERAL_PICKERS.lock().unwrap_or_else(|e| e.into_inner());
    q.iter().find(|(c, _)| *c == chat_id).map(|(_, id)| *id)
}

/// Drop the tracked picker for a chat.
///
/// Returns the id it held so a caller that wants to remove the bubble can.
/// The edit path only needs the tracking to stop: the picker it just edited
/// has had its buttons stripped, so nothing can tap it again and keeping the
/// id would let a later tap edit a message that is already finished.
pub(crate) fn forget_picker(chat_id: i64) -> Option<i64> {
    let mut q = EPHEMERAL_PICKERS.lock().unwrap_or_else(|e| e.into_inner());
    let idx = q.iter().position(|(c, _)| *c == chat_id)?;
    q.remove(idx).map(|(_, id)| id)
}

/// The user an ephemeral reply should be scoped to, or `None` when the reply
/// must stay an ordinary message.
///
/// A DM is already private and has no other members to hide the reply from,
/// so `receiver_user_id` buys nothing there, and asking for it would trade a
/// working plain send for an untested one.
pub(crate) fn receiver_for(is_dm: bool, user_id: i64) -> Option<i64> {
    if is_dm { None } else { Some(user_id) }
}

/// Build the `sendMessage` body for an ephemeral reply. Split out from the
/// transport so the request shape is unit-testable without a live bot.
///
/// `parse_html` mirrors the caller's existing choice: command output that
/// went through `command_md_to_html` needs `parse_mode`, bare status acks
/// must not have it or their `<`/`&` would be read as markup.
///
/// Bot API 10.3 (2026-08-24) replaced the flat `receiver_user_id` with the
/// `ephemeral_message_parameters` object, so this builds the current shape and
/// [`build_body_legacy`] keeps the pre-10.3 one for the fallback in
/// [`send_one_scoped`]. Both go through [`finish_body`] so the two can only
/// differ in how they scope, never in how they carry text.
pub(crate) fn build_body(
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    text: &str,
    parse_html: bool,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "ephemeral_message_parameters": {"receiver_user_id": receiver_user_id},
    });
    finish_body(&mut body, thread_id, parse_html);
    body
}

/// The pre-10.3 shape: the scoping id as a flat top-level parameter. Kept as
/// the one-shot fallback for a server that predates the object, because
/// whether the replacement kept the old parameter working is not something
/// this client can settle from here.
pub(crate) fn build_body_legacy(
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    text: &str,
    parse_html: bool,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "receiver_user_id": receiver_user_id,
    });
    finish_body(&mut body, thread_id, parse_html);
    body
}

/// The part of a scoped body that has nothing to do with scoping.
fn finish_body(body: &mut serde_json::Value, thread_id: Option<ThreadId>, parse_html: bool) {
    if parse_html {
        body["parse_mode"] = serde_json::json!("HTML");
    }
    if let Some(t) = thread_id {
        // ThreadId wraps a MessageId(i32).
        body["message_thread_id"] = serde_json::json!(t.0.0);
    }
}

/// Which shape of the scoping parameter this server accepts.
///
/// Bot API 10.3 replaced the flat `receiver_user_id` with the
/// `ephemeral_message_parameters` object, but a replacement in the changelog
/// is not proof that the old parameter stopped working, and this client cannot
/// ask the server that question without sending something. So the answer is
/// learned at runtime, the same way [`RICH_SCOPING`] learns its own: try the
/// current shape, fall back to the flat one once on a rejection, and remember
/// the verdict for the life of the process.
static SCOPING_SHAPE: AtomicU8 = AtomicU8::new(SHAPE_UNKNOWN);
const SHAPE_UNKNOWN: u8 = 0;
const SHAPE_OBJECT: u8 = 1;
const SHAPE_FLAT: u8 = 2;

/// What a scoped send produced: whether it landed, and the id needed to edit
/// or delete it later.
///
/// The two are separate on purpose. `ephemeral_message_id` is optional in the
/// `Message` object, so a server may accept the send and echo no id; treating
/// that as a failure would send the same reply a second time on the public
/// path. Only `landed` decides that.
pub(crate) struct ScopedSend {
    pub landed: bool,
    pub ephemeral_message_id: Option<i64>,
}

/// Pull `result.ephemeral_message_id` out of a `sendMessage` response.
pub(crate) fn ephemeral_id_from(parsed: &serde_json::Value) -> Option<i64> {
    parsed
        .get("result")
        .and_then(|r| r.get("ephemeral_message_id"))
        .and_then(serde_json::Value::as_i64)
}

/// Deliver one ephemeral reply, reporting whether it landed and under which id.
///
/// A rejection of the object shape retries once with the flat one before the
/// caller is told to send publicly, because the two failures look identical
/// from here and only one of them is about the parameter's shape.
pub(crate) async fn send_one_scoped(
    token: &str,
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    text: &str,
    parse_html: bool,
    markup: Option<&serde_json::Value>,
) -> ScopedSend {
    let flat = SCOPING_SHAPE.load(Ordering::Relaxed) == SHAPE_FLAT;
    let mut body = if flat {
        build_body_legacy(chat_id, thread_id, receiver_user_id, text, parse_html)
    } else {
        build_body(chat_id, thread_id, receiver_user_id, text, parse_html)
    };
    if let Some(m) = markup {
        body["reply_markup"] = m.clone();
    }
    let (outcome, parsed) = post_json(token, "sendMessage", &body).await;
    match outcome {
        Outcome::Sent => {
            if !flat && SCOPING_SHAPE.load(Ordering::Relaxed) == SHAPE_UNKNOWN {
                SCOPING_SHAPE.store(SHAPE_OBJECT, Ordering::Relaxed);
            }
            ScopedSend {
                landed: true,
                ephemeral_message_id: ephemeral_id_from(&parsed),
            }
        }
        Outcome::Rejected if !flat => {
            // The object shape was refused. One retry with the pre-10.3 form
            // settles whether that was about the shape or about this chat.
            let mut legacy =
                build_body_legacy(chat_id, thread_id, receiver_user_id, text, parse_html);
            if let Some(m) = markup {
                legacy["reply_markup"] = m.clone();
            }
            let (retry, retry_parsed) = post_json(token, "sendMessage", &legacy).await;
            if retry == Outcome::Sent {
                SCOPING_SHAPE.store(SHAPE_FLAT, Ordering::Relaxed);
                tracing::info!(
                    "Telegram: ephemeral scoping wants the flat receiver_user_id, \
                     not the 10.3 ephemeral_message_parameters object"
                );
                ScopedSend {
                    landed: true,
                    ephemeral_message_id: ephemeral_id_from(&retry_parsed),
                }
            } else {
                // Neither shape worked, so the shape is not the question. Pin
                // the cheaper one-call form and stop paying for the probe.
                SCOPING_SHAPE.store(SHAPE_FLAT, Ordering::Relaxed);
                ScopedSend {
                    landed: false,
                    ephemeral_message_id: None,
                }
            }
        }
        _ => ScopedSend {
            landed: false,
            ephemeral_message_id: None,
        },
    }
}

/// Deliver one ephemeral reply. `true` when it landed; `false` means the
/// caller must send `text` publicly as it did before 10.2.
pub(crate) async fn send_one(
    token: &str,
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    text: &str,
    parse_html: bool,
) -> bool {
    send_one_scoped(
        token,
        chat_id,
        thread_id,
        receiver_user_id,
        text,
        parse_html,
        None,
    )
    .await
    .landed
}

/// Body for `editEphemeralMessageText`, the only way to change a scoped reply
/// after it was sent. `message_id` is 0 for an ephemeral message, so the
/// ordinary `editMessageText` cannot address it.
pub(crate) fn build_edit_text_body(
    chat_id: i64,
    ephemeral_message_id: i64,
    text: &str,
    parse_html: bool,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "chat_id": chat_id,
        "ephemeral_message_id": ephemeral_message_id,
        "text": text,
    });
    if parse_html {
        body["parse_mode"] = serde_json::json!("HTML");
    }
    body
}

/// Body for `editEphemeralMessageReplyMarkup`: swap the buttons on a scoped
/// reply without resending its text.
pub(crate) fn build_edit_markup_body(
    chat_id: i64,
    ephemeral_message_id: i64,
    markup: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "chat_id": chat_id,
        "ephemeral_message_id": ephemeral_message_id,
        "reply_markup": markup,
    })
}

/// Body for `deleteEphemeralMessage`.
pub(crate) fn build_delete_body(chat_id: i64, ephemeral_message_id: i64) -> serde_json::Value {
    serde_json::json!({
        "chat_id": chat_id,
        "ephemeral_message_id": ephemeral_message_id,
    })
}

/// Rewrite a scoped reply in place. `true` when the server accepted it.
pub(crate) async fn edit_text(
    token: &str,
    chat_id: i64,
    ephemeral_message_id: i64,
    text: &str,
    parse_html: bool,
) -> bool {
    let body = build_edit_text_body(chat_id, ephemeral_message_id, text, parse_html);
    post(token, "editEphemeralMessageText", &body).await == Outcome::Sent
}

/// Replace only the keyboard of a scoped reply.
pub(crate) async fn edit_reply_markup(
    token: &str,
    chat_id: i64,
    ephemeral_message_id: i64,
    markup: &serde_json::Value,
) -> bool {
    let body = build_edit_markup_body(chat_id, ephemeral_message_id, markup);
    post(token, "editEphemeralMessageReplyMarkup", &body).await == Outcome::Sent
}

/// Remove a scoped reply. Used when the command it belonged to is done, so a
/// finished picker does not linger in the chat it was scoped to.
pub(crate) async fn delete_message(token: &str, chat_id: i64, ephemeral_message_id: i64) -> bool {
    let body = build_delete_body(chat_id, ephemeral_message_id);
    post(token, "deleteEphemeralMessage", &body).await == Outcome::Sent
}

/// Build the scoped `sendRichMessage` body: exactly what the public rich path
/// sends, plus the scoping object. Split out so the shape stays testable and so
/// the two paths cannot drift.
pub(crate) fn build_rich_body(
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    markdown: &str,
) -> serde_json::Value {
    let mut body = super::rich::api::build_body(chat_id, thread_id, markdown, None);
    body["ephemeral_message_parameters"] =
        serde_json::json!({"receiver_user_id": receiver_user_id});
    body
}

/// The pre-10.3 scoped rich body, for the one-shot fallback in
/// [`try_send_rich`].
pub(crate) fn build_rich_body_legacy(
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    markdown: &str,
) -> serde_json::Value {
    let mut body = super::rich::api::build_body(chat_id, thread_id, markdown, None);
    body["receiver_user_id"] = serde_json::json!(receiver_user_id);
    body
}

/// Whether `sendRichMessage` accepts `receiver_user_id`, as answered by the
/// server rather than assumed here. See [`try_send_rich`].
static RICH_SCOPING: AtomicU8 = AtomicU8::new(RICH_UNKNOWN);
const RICH_UNKNOWN: u8 = 0;
const RICH_SUPPORTED: u8 = 1;
const RICH_UNSUPPORTED: u8 = 2;

/// Try to deliver `markdown` as a *native rich* message scoped to one user,
/// so a table or heading keeps its real Telegram rendering while staying
/// private. `true` when it landed.
///
/// The 10.2 changelog enumerates the methods that gained `receiver_user_id`
/// (`sendMessage` and the media senders) and `sendRichMessage` is not among
/// them, but that method's own parameter table was never available to confirm
/// the omission. So this asks the server instead of hard-coding the guess: the
/// first group reply attempts it, and the answer is remembered for the life of
/// the process. If the API does support it, every later reply gets native rich
/// blocks *and* privacy; if not, exactly one call is wasted before the HTML
/// path takes over for good.
///
/// A transport error leaves the answer `UNKNOWN` on purpose: a dropped
/// connection is not the server declining the parameter, and caching it as one
/// would forfeit native rich for the rest of the process over a network blip.
pub(crate) async fn try_send_rich(
    token: &str,
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    markdown: &str,
) -> bool {
    if RICH_SCOPING.load(Ordering::Relaxed) == RICH_UNSUPPORTED {
        return false;
    }
    let flat = SCOPING_SHAPE.load(Ordering::Relaxed) == SHAPE_FLAT;
    let body = if flat {
        build_rich_body_legacy(chat_id, thread_id, receiver_user_id, markdown)
    } else {
        build_rich_body(chat_id, thread_id, receiver_user_id, markdown)
    };
    match post(token, "sendRichMessage", &body).await {
        Outcome::Sent => {
            if !flat && SCOPING_SHAPE.load(Ordering::Relaxed) == SHAPE_UNKNOWN {
                SCOPING_SHAPE.store(SHAPE_OBJECT, Ordering::Relaxed);
            }
            if RICH_SCOPING.swap(RICH_SUPPORTED, Ordering::Relaxed) == RICH_UNKNOWN {
                tracing::info!(
                    "Telegram: sendRichMessage accepts a scoping parameter, \
                     scoped command replies keep native rich rendering"
                );
            }
            true
        }
        Outcome::Rejected if !flat => {
            // Same one-shot probe as [`send_one_scoped`]: the two rejections
            // are indistinguishable from here, so retry the flat shape once
            // before giving up on rich scoping entirely.
            let legacy = build_rich_body_legacy(chat_id, thread_id, receiver_user_id, markdown);
            match post(token, "sendRichMessage", &legacy).await {
                Outcome::Sent => {
                    SCOPING_SHAPE.store(SHAPE_FLAT, Ordering::Relaxed);
                    RICH_SCOPING.store(RICH_SUPPORTED, Ordering::Relaxed);
                    tracing::info!(
                        "Telegram: sendRichMessage wants the flat receiver_user_id, \
                         not the 10.3 ephemeral_message_parameters object"
                    );
                    true
                }
                _ => {
                    RICH_SCOPING.store(RICH_UNSUPPORTED, Ordering::Relaxed);
                    tracing::info!(
                        "Telegram: sendRichMessage does not accept scoping, \
                         scoped command replies will render as HTML from here on"
                    );
                    false
                }
            }
        }
        Outcome::Rejected => {
            RICH_SCOPING.store(RICH_UNSUPPORTED, Ordering::Relaxed);
            tracing::info!(
                "Telegram: sendRichMessage does not accept scoping, \
                 scoped command replies will render as HTML from here on"
            );
            false
        }
        Outcome::Transport => false,
    }
}

/// Deliver `chunks` in order, scoped to `receiver_user_id`.
///
/// Returns how many *leading* chunks landed, so the caller can finish the
/// message publicly without duplicating what already arrived: `0` means fall
/// back wholesale, a short count means public-send `chunks[n..]`. Stops at the
/// first failure rather than interleaving delivered and dropped chunks, which
/// would reorder a split reply.
pub(crate) async fn send_html_chunks(
    token: &str,
    chat_id: i64,
    thread_id: Option<ThreadId>,
    receiver_user_id: i64,
    chunks: &[&str],
) -> usize {
    let mut delivered = 0usize;
    for chunk in chunks {
        if !send_one(token, chat_id, thread_id, receiver_user_id, chunk, true).await {
            break;
        }
        delivered += 1;
    }
    delivered
}

/// Send a one-off command ack: scoped to `receiver` when there is one, and
/// on the ordinary rate-limit-retrying public path otherwise.
///
/// `receiver` comes from [`receiver_for`], so `None` (a DM) skips straight to
/// the public send. Acks are plain text: they carry no markup and must not be
/// parsed as HTML.
pub(crate) async fn send_ack(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    receiver: Option<i64>,
    text: &str,
) -> std::result::Result<(), teloxide::RequestError> {
    if let Some(rx) = receiver
        && send_one(bot.token(), chat_id.0, thread_id, rx, text, false).await
    {
        return Ok(());
    }
    super::intermediates::send_retrying_rate_limit("command reply", || {
        super::send::message_in_thread(bot, chat_id, thread_id, text)
    })
    .await
    .map(|_| ())
}

/// What the server did with an ephemeral send.
///
/// `Rejected` and `Transport` both leave the caller sending publicly, but only
/// `Rejected` is the server stating an opinion. A dropped connection says
/// nothing about what the API supports, so the two must not be conflated when
/// caching a capability.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Sent,
    Rejected,
    Transport,
}

/// POST an ephemeral send to `method`, logging why it did not land.
///
/// A 400 here is the expected shape of "this server predates 10.2" or "the
/// bot may not scope messages in this chat", so it is a warning and the
/// caller recovers, but it is never swallowed, because a silent failure
/// looks identical to the feature working while every reply stays public.
///
/// A 429 is neither an opinion nor a plain transport failure: it is throttling.
/// Waiting out the server's `retry_after` (capped) and retrying once usually
/// lands the send; if it does not, the outcome is `Transport`, never
/// `Rejected` — caching a throttle as a capability verdict would forfeit the
/// feature for the life of the process over a busy minute (the exact mistake
/// `Outcome`'s doc warns about).
async fn post_json(
    token: &str,
    method: &str,
    body: &serde_json::Value,
) -> (Outcome, serde_json::Value) {
    const RETRY_AFTER_CAP: std::time::Duration = std::time::Duration::from_secs(15);
    let url = format!("https://api.telegram.org/bot{token}/{method}");
    let mut resp = match reqwest::Client::new().post(&url).json(body).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Telegram: ephemeral {method} transport error: {e}, caller falls back");
            return (Outcome::Transport, serde_json::Value::Null);
        }
    };
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let text = resp.text().await.unwrap_or_default();
        let retry_after = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("parameters")
                    .and_then(|p| p.get("retry_after"))
                    .and_then(serde_json::Value::as_u64)
            })
            .map(std::time::Duration::from_secs)
            .unwrap_or(RETRY_AFTER_CAP)
            .min(RETRY_AFTER_CAP);
        tracing::warn!(
            "Telegram: ephemeral {method} throttled, waiting {retry_after:?} and retrying once"
        );
        tokio::time::sleep(retry_after).await;
        resp = match reqwest::Client::new().post(&url).json(body).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "Telegram: ephemeral {method} transport error on retry: {e}, caller falls back"
                );
                return (Outcome::Transport, serde_json::Value::Null);
            }
        };
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            tracing::warn!(
                "Telegram: ephemeral {method} still throttled after retry, caller falls back"
            );
            return (Outcome::Transport, serde_json::Value::Null);
        }
    }
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();

    if status.is_success() && parsed.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return (Outcome::Sent, parsed);
    }

    let desc = parsed
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&text);
    tracing::warn!("Telegram: ephemeral {method} rejected ({status}): {desc}, caller falls back");
    (Outcome::Rejected, parsed)
}

/// [`post_json`] for callers that only care whether it landed.
async fn post(token: &str, method: &str, body: &serde_json::Value) -> Outcome {
    post_json(token, method, body).await.0
}
