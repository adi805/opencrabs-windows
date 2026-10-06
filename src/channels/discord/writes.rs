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

use serenity::builder::{CreateMessage, EditMessage};
use serenity::http::Http;
use serenity::model::channel::Message;
use serenity::model::id::{ChannelId, MessageId};

use super::governor::{self, Admission, WriteClass};

/// Re-exported so call sites can name the class without importing the
/// governor module directly: `super::writes::Class::Edit`.
pub(crate) use super::governor::WriteClass as Class;

/// Feed a rate-limit rejection back into the governor so the next write to
/// this channel waits out the cooldown instead of hammering.
fn note_if_rate_limited(channel: ChannelId, result: &serenity::Result<Message>) {
    if let Err(e) = result {
        let text = e.to_string();
        if governor::is_rate_limited(&text) {
            let retry_after = governor::parse_retry_after(&text);
            governor::record_429(channel.get(), retry_after);
            // The cooldown actually armed is the load-bearing number for
            // diagnosing a starved channel, so log it rather than the input.
            tracing::warn!(
                channel = channel.get(),
                retry_after_ms = retry_after.map(|d| d.as_millis() as u64),
                cooldown_ms =
                    governor::cooldown_remaining(channel.get()).map(|d| d.as_millis() as u64),
                count_429 = governor::snapshot(channel.get()).map(|s| s.count_429),
                "Discord write rate limited; backing off"
            );
        }
    }
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
