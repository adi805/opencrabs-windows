//! Inline mode: the one update kind that can arrive from a chat the bot was
//! never added to (#99, #109).
//!
//! Every other handler in this module serves a chat the bot was invited to, so
//! the group and message allowlists are the gate. An inline query has no chat
//! and no membership: it can be typed into any conversation on Telegram by any
//! user, and Telegram shows the bot's answer to whoever typed it. That makes
//! the answer surface wider than the rest of the channel, which is why this
//! module is deliberately narrow:
//!
//! - The result set is a fixed table of command strings, not generated text,
//!   so an inline query can never start a turn or spend tokens. Issue #99 asks
//!   for exactly this: a bounded set of results rather than a free-form
//!   generation, because an unbounded handler is a second agent loop with a
//!   wider reach than the first one.
//! - Nothing from the session, workspace, or history is rendered into a
//!   result. A result is a string a user can paste, and a pasted command from
//!   a session title or file path would carry private context into a chat the
//!   owner never chose to expose it in.
//! - Non-owners get one result that says the bot is owner-only. Silently
//!   answering with nothing is indistinguishable from a broken handler, which
//!   is the failure mode #99 called out.
//!
//! The answer goes out over raw HTTP for the same reason the ephemeral path
//! does: the body is then a plain value that can be unit-tested without a live
//! bot, and nothing depends on a binding this box cannot inspect.

/// Commands an inline result may hand out.
///
/// Bounded on purpose: a paste-ready command is useful in any chat, while a
/// rendered answer would need session context that must not travel. Kept small
/// so the whole result set stays far inside Telegram's payload limit, and
/// pinned to [`crate::channels::commands::format_help`] by a test so this
/// table cannot drift away from the command list users actually have.
pub(crate) const INLINE_COMMANDS: &[(&str, &str)] = &[
    ("/help", "Show the command list"),
    ("/models", "Switch AI model"),
    ("/sessions", "Switch between sessions"),
    ("/new", "Start a new session"),
    ("/audit", "Audit trail viewer"),
    ("/plan", "Enter Plan mode (design a plan for approval)"),
    ("/mission-control", "Analytics, activity, inbox & schedule"),
];

/// What a non-owner is told. Short enough to read inside the inline picker,
/// and it names the reason so the answer is not mistaken for an empty result.
pub(crate) const OWNER_ONLY_NOTICE: &str = "OpenCrabs is owner-only. Ask the bot owner for access.";

/// The inline result set for one query.
///
/// `is_owner` is decided by the caller from the same config check the rest of
/// the channel uses, and the two branches share no code: the owner branch
/// renders the command table, and every other user gets the notice alone.
pub(crate) fn build_results(is_owner: bool) -> serde_json::Value {
    if !is_owner {
        return serde_json::json!([article(
            "owner-only",
            "OpenCrabs is owner-only",
            OWNER_ONLY_NOTICE,
            OWNER_ONLY_NOTICE,
        )]);
    }
    let results: Vec<serde_json::Value> = INLINE_COMMANDS
        .iter()
        .map(|(cmd, description)| {
            article(
                &format!("cmd:{cmd}"),
                cmd,
                description,
                // The pasted text is the command itself, so inserting it into
                // a chat with the bot produces the command rather than a
                // sentence about the command.
                cmd,
            )
        })
        .collect();
    serde_json::Value::Array(results)
}

/// One article result: what the picker shows, and what gets inserted.
fn article(id: &str, title: &str, description: &str, message_text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "article",
        "id": id,
        "title": title,
        "description": description,
        "input_message_content": {"message_text": message_text},
    })
}

/// The `answerInlineQuery` body.
///
/// `is_personal` is true because the result set depends on who asked: Telegram
/// caches inline answers per query text otherwise, and a cached owner answer
/// served to another user is exactly the leak this module exists to prevent.
/// `cache_time` 0 keeps that from being papered over by a long TTL.
pub(crate) fn build_answer_body(query_id: &str, results: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "inline_query_id": query_id,
        "results": results,
        "cache_time": 0,
        "is_personal": true,
    })
}

/// Answer one inline query. `true` when Telegram accepted the answer.
///
/// Nothing is retried here. An inline query expires in seconds, so a retry
/// would either answer a query the user has already stopped looking at or race
/// the next keystroke; a failure is logged and dropped instead.
pub(crate) async fn answer(token: &str, query_id: &str, results: &serde_json::Value) -> bool {
    let body = build_answer_body(query_id, results);
    let url = format!("https://api.telegram.org/bot{token}/answerInlineQuery");
    let resp = match reqwest::Client::new().post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Telegram: inline answer transport error: {e}");
            return false;
        }
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    if status.is_success() && parsed.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return true;
    }
    let desc = parsed
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&text);
    tracing::warn!("Telegram: inline answer rejected ({status}): {desc}");
    false
}

/// Decide the result set and answer in one step.
pub(crate) async fn answer_query(token: &str, query_id: &str, is_owner: bool) -> bool {
    answer(token, query_id, &build_results(is_owner)).await
}
