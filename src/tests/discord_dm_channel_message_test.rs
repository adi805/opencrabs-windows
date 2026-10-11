//! A Discord DM turn must reach `channel_messages` (#200).
//!
//! `channel_search` is built on `ChannelMessageRepository` alone, with no
//! fallback to the session `messages` table. So a turn that never lands in
//! `channel_messages` is not merely under-documented: it is invisible to all
//! four `channel_search` operations (`list_chats`, `recent`, `search`,
//! `attachments`), and the Discord DM path has no message-id lookup either.
//!
//! Telegram in the same tree stores its DM replies. Its guard is
//! `(!is_dm || pmid.is_some())`, so a DM reply that has an id is persisted.
//! Discord guarded both write paths on a bare `!is_dm`, so a DM turn was
//! dropped outright while the guild path worked. The two channels disagreed
//! about the same situation: a Telegram DM is searchable, a Discord DM is not.
//!
//! The skip was deliberate, and its reasoning still holds for context
//! injection (the session table carries the current turn). It does not hold for
//! retrieval, which is what these tests pin.
//!
//! These are source-shape tests: both stores live inside the `handle_message`
//! body behind live `Http` calls, so neither arm is reachable from a unit test.
//! A doc comment cannot fail a build when someone re-adds the guard; these can.

use crate::tests::source_shape::{flatten, read_in};

/// `handler.rs` of the discord channel module, read from the crate root.
fn read_handler() -> String {
    read_in("src/channels/discord", "handler.rs")
}

/// The inbound store: from its comment through the store call it guards.
fn inbound_store_arm() -> String {
    let handler = read_handler();
    let start = handler
        .find("Also store directed channel messages for complete history")
        .expect("the inbound channel_messages store moved: #200 cannot pin it");
    let rest = &handler[start..];
    let call = "store_channel_msg(msg.content.clone()).await;";
    let end = rest
        .find(call)
        .expect("the inbound store call moved: #200 cannot pin it")
        + call.len();
    flatten(&rest[..end])
}

/// The outbound store: from its comment through the row builder it reaches.
fn outbound_store_arm() -> String {
    let handler = read_handler();
    let start = handler
        .find("Record the bot's reply in channel_messages")
        .expect("the outbound channel_messages store moved: #200 cannot pin it");
    let rest = &handler[start..];
    let call = "DbChannelMessage::new(";
    let end = rest
        .find(call)
        .expect("the outbound row builder moved: #200 cannot pin it")
        + call.len();
    flatten(&rest[..end])
}

#[test]
fn a_dm_inbound_message_reaches_channel_messages() {
    let arm = inbound_store_arm();
    assert!(
        arm.contains("store_channel_msg("),
        "the inbound store call is gone, so no message reaches \
         channel_messages at all (#200). arm:\n{arm}"
    );
    assert!(
        !arm.contains("is_dm"),
        "the inbound store is gated on is_dm again: a Discord DM turn \
         vanishes from channel_messages and channel_search goes blind in DMs \
         while Telegram stores the same turn (#200). arm:\n{arm}"
    );
}

#[test]
fn a_dm_bot_reply_reaches_channel_messages() {
    let arm = outbound_store_arm();
    assert!(
        arm.contains("DbChannelMessage::new("),
        "the outbound row builder is gone, so the bot's reply no longer \
         reaches channel_messages (#200). arm:\n{arm}"
    );
    assert!(
        arm.contains("text_only.trim().is_empty()"),
        "the empty-reply guard is gone: #200 pins that only a non-empty \
         reply is stored. arm:\n{arm}"
    );
    assert!(
        !arm.contains("is_dm"),
        "the outbound store is gated on is_dm again: a Discord DM reply \
         vanishes from channel_messages and channel_search goes blind in DMs \
         while Telegram stores the same reply (#200). arm:\n{arm}"
    );
}
