//! #2025: a Discord DM turn reaches `channel_messages` on both sides, so
//! `channel_search` can find DM conversations as it can for Telegram, Slack
//! and WhatsApp. The message path needs a live gateway, so the guards are
//! pinned against the source, the house pattern for these handlers.

const HANDLER: &str = include_str!("../channels/discord/handler.rs");

#[test]
fn inbound_dm_messages_are_stored_too() {
    assert!(
        !HANDLER.contains("if !is_dm {\n        store_channel_msg(msg.content.clone()).await;"),
        "the inbound store must not be guarded by !is_dm"
    );
    assert!(
        HANDLER.contains("store_channel_msg(msg.content.clone()).await;\n"),
        "the directed message is stored unconditionally"
    );
}

#[test]
fn a_dm_reply_is_stored_when_it_has_a_message_id() {
    assert!(
        HANDLER.contains("if (!is_dm || reply_msg_id.is_some()) && !text_only.trim().is_empty()"),
        "the bot reply is stored for DMs when it has an id, as Telegram does"
    );
    assert!(
        HANDLER.contains("reply_msg_id.clone(),"),
        "the reply's platform message id is recorded for lookup"
    );
    assert!(
        HANDLER.contains("Ok(m) => reply_msg_id = Some(m.id.get().to_string()),"),
        "the id is taken from the sent chunk, not guessed"
    );
}
