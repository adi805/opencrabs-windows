//! Governed Discord write helpers (PRD FR-003).
//!
//! Every repeated Discord write goes through here so the [`governor`] sees it:
//! the helper asks for admission first, performs the serenity call, and feeds a
//! 429 back into the governor's cooldown ladder.
//!
//! # Return shape
//!
//! `Ok(Some(msg))` — the write landed.
//! `Ok(None)` — the governor REFUSED it: the class is droppable and the budget
//! is spent, so the caller skips the write and carries on. Dropping is safe by
//! design for cosmetic classes — the next refresh re-renders the FULL current
//! state, so the dropped content rides the next admitted write.
//! `Err(e)` — the transport failed, exactly as the raw serenity call would
//! have reported it.
//!
//! Keeping the error in the `Err` position (rather than wrapping the whole
//! thing in an `Option`) means every existing call site keeps its
//! `if let Err(e) = ...` shape: only the receiver and arguments move.
//!
//! One-shot command replies (`/help`, `/usage`, `/stop`, ...) ARE routed here
//! as [`WriteClass::Final`], so a 429 on one of them teaches the governor and
//! widens that channel's cooldown. `Final` is never dropped, so a reply cannot
//! vanish: worst case it waits out the cooldown and then fails open.

use serenity::builder::{CreateMessage, EditMessage, ExecuteWebhook};
use serenity::http::Http;
use serenity::model::channel::Message;
use serenity::model::id::{ChannelId, MessageId};
use serenity::model::webhook::Webhook;

use super::governor::{self, Admission, WriteClass};

/// Re-exported so call sites can name the class without importing the
/// governor module directly: `super::writes::Class::Edit`.
pub(crate) use super::governor::WriteClass as Class;

/// Feed a rate-limit rejection back into the governor so the next write to
/// this channel waits out the cooldown instead of hammering.
fn note_if_rate_limited(channel: ChannelId, result: &serenity::Result<Message>) {
    if let Err(e) = result {
        note_rate_limited(channel, &e.to_string());
    }
}

/// The cooldown ladder body, shared by every governed helper.
///
/// Split out from [`note_if_rate_limited`] because the helpers do not agree on
/// their result shape: the channel writes return `Result<Message>` while
/// [`execute_webhook`] returns `Result<Option<Message>>`, which is the shape
/// serenity gives that call. Taking the error text keeps one ladder rather
/// than two copies that drift apart.
fn note_rate_limited(channel: ChannelId, text: &str) {
    if !governor::is_rate_limited(text) {
        return;
    }
    let retry_after = governor::parse_retry_after(text);
    governor::record_429(channel.get(), retry_after);
    // The cooldown actually armed is the load-bearing number for diagnosing a
    // starved channel, so log it rather than the input.
    tracing::warn!(
        channel = channel.get(),
        retry_after_ms = retry_after.map(|d| d.as_millis() as u64),
        cooldown_ms = governor::cooldown_remaining(channel.get()).map(|d| d.as_millis() as u64),
        count_429 = governor::snapshot(channel.get()).map(|s| s.count_429),
        "Discord write rate limited; backing off"
    );
}

/// Record a governed drop. Cosmetic drops are expected under load, so this is
/// debug-level: it exists to make a starving channel visible in the log.
fn note_drop(channel: ChannelId, class: WriteClass) {
    if let Some(snap) = governor::snapshot(channel.get()) {
        tracing::debug!(
            channel = channel.get(),
            ?class,
            dropped_total = snap.dropped,
            throttled_ms = snap.throttled_ms,
            "Discord write dropped by budget governor"
        );
    }
}

/// Governed `ChannelId::say`.
#[allow(clippy::result_large_err)]
pub(crate) async fn say(
    http: &Http,
    channel: ChannelId,
    content: impl AsRef<str>,
    class: WriteClass,
) -> serenity::Result<Option<Message>> {
    if governor::admit(channel.get(), class).await == Admission::Drop {
        note_drop(channel, class);
        return Ok(None);
    }
    let res = channel.say(http, content.as_ref()).await;
    note_if_rate_limited(channel, &res);
    res.map(Some)
}

/// Governed `ChannelId::send_message`.
#[allow(clippy::result_large_err)]
pub(crate) async fn send(
    http: &Http,
    channel: ChannelId,
    builder: CreateMessage,
    class: WriteClass,
) -> serenity::Result<Option<Message>> {
    if governor::admit(channel.get(), class).await == Admission::Drop {
        note_drop(channel, class);
        return Ok(None);
    }
    let res = channel.send_message(http, builder).await;
    note_if_rate_limited(channel, &res);
    res.map(Some)
}

/// Governed `ChannelId::edit_message`.
#[allow(clippy::result_large_err)]
pub(crate) async fn edit(
    http: &Http,
    channel: ChannelId,
    message_id: MessageId,
    builder: EditMessage,
    class: WriteClass,
) -> serenity::Result<Option<Message>> {
    if governor::admit(channel.get(), class).await == Admission::Drop {
        note_drop(channel, class);
        return Ok(None);
    }
    let res = channel.edit_message(http, message_id, builder).await;
    note_if_rate_limited(channel, &res);
    res.map(Some)
}

/// Governed [`Webhook::execute`] (PRD FR-012).
///
/// An announcement leaves through a webhook rather than the bot's own channel
/// write, but it is still a message in a channel: it spends the same budget and
/// has to teach the same 429 ladder, so it goes through here instead of calling
/// `Webhook::execute` directly.
///
/// `wait = true` is what makes the return value useful. Without it Discord
/// answers `204` and hands back no message, and there would be nothing to
/// crosspost. The `Option` is serenity's shape for that call rather than a drop
/// signal of its own: a governor refusal returns `Ok(None)` before the call is
/// made, while an admitted call returns `Ok(Some(msg))`.
///
/// The class is fixed at [`WriteClass::Final`] rather than taken from the
/// caller: an announcement carries content and has no second render to heal a
/// drop, which is the definition of `Final`. Taking a parameter here would only
/// invite a caller to classify it as cosmetic.
#[allow(clippy::result_large_err)]
pub(crate) async fn execute_webhook(
    http: &Http,
    channel: ChannelId,
    webhook: &Webhook,
    builder: ExecuteWebhook,
) -> serenity::Result<Option<Message>> {
    if governor::admit(channel.get(), WriteClass::Final).await == Admission::Drop {
        note_drop(channel, WriteClass::Final);
        return Ok(None);
    }
    let res = webhook.execute(http, true, builder).await;
    if let Err(e) = &res {
        note_rate_limited(channel, &e.to_string());
    }
    res
}
