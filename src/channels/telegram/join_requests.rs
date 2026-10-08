//! Chat join request handling (#PR4).
//!
//! A join request comes from a user who is NOT in the chat yet, and the Bot API
//! never delivers it as a `message` update, so before this module the request
//! was invisible twice over: Telegram sent nothing (the kind was absent from
//! `ALLOWED_UPDATES`) and, had it arrived, the dispatcher had no branch for it
//! and would have dropped it.
//!
//! The handler OBSERVES and REPORTS only. Approving grants chat access, which
//! is not reversible by the agent, so it stays a deliberate owner action
//! through `telegram_send`'s `approve_chat_join_request` /
//! `decline_chat_join_request` rather than something that fires on arrival.

use teloxide::Bot;
use teloxide::types::ChatJoinRequest;

use crate::config::Config;

/// Handle one `chat_join_request` update.
///
/// Best-effort by design: a request that cannot be reported is logged, never
/// propagated, so a delivery failure cannot take down the dispatcher loop.
pub(crate) async fn handle_join_request(bot: &Bot, req: &ChatJoinRequest, cfg: &Config) {
    let chat_id = req.chat.id.0;
    let chat_title = req.chat.title().unwrap_or("unknown");
    let user_id = req.from.id.0 as i64;
    let name = req.from.username.as_deref().unwrap_or(&req.from.first_name);

    tracing::info!(
        "Telegram: join request for \"{}\" (chat_id={}) from user_id={} username={} \
         invite_link={}",
        chat_title,
        chat_id,
        user_id,
        name,
        req.invite_link
            .as_ref()
            .map(|l| l.invite_link.as_str())
            .unwrap_or("-"),
    );

    // Report to the owner's DM. The notice carries exactly the two ids
    // `approve_chat_join_request` / `decline_chat_join_request` take, so the
    // owner can act without a second lookup.
    let Some(owner_id_str) = cfg.channels.telegram.allowed_users.first() else {
        return;
    };
    let Ok(owner_id) = owner_id_str.parse::<i64>() else {
        return;
    };
    let note = format_join_request_notification(
        chat_title,
        chat_id,
        req.chat.username(),
        name,
        user_id,
        req.bio.as_deref(),
    );
    if let Err(e) =
        super::send::message_in_thread(bot, teloxide::types::ChatId(owner_id), None, note).await
    {
        tracing::error!(
            "Telegram: could not tell the owner about a join request for \"{}\" \
             (chat_id={}, user_id={}), so it is unreported: {}",
            chat_title,
            chat_id,
            user_id,
            e
        );
    }
}

/// Format the owner's notification for a join request.
///
/// `chat_id` + `user_id` are the whole payload the owner needs, so they are
/// printed in both the summary and the two ready-to-paste action lines.
fn format_join_request_notification(
    chat_title: &str,
    chat_id: i64,
    chat_username: Option<&str>,
    user_name: &str,
    user_id: i64,
    bio: Option<&str>,
) -> String {
    let where_ = match chat_username {
        Some(u) if !u.is_empty() => {
            format!("\"{chat_title}\" (chat_id={chat_id}, https://t.me/{u})")
        }
        _ => format!("\"{chat_title}\" (chat_id={chat_id})"),
    };
    let bio_line = match bio.map(str::trim) {
        Some(b) if !b.is_empty() => format!("\nBio: {b}"),
        _ => String::new(),
    };
    format!(
        "🙋 Join request for {where_} from {user_name} (user_id={user_id}).{bio_line}\n\
         Approve: telegram_send approve_chat_join_request chat_id={chat_id} user_id={user_id}\n\
         Decline: telegram_send decline_chat_join_request chat_id={chat_id} user_id={user_id}"
    )
}
