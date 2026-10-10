//! Discord auto-embed: every outbound message becomes a card.
//!
//! `[channels.discord] auto_embed` (default true) rewrites a text-only message
//! into a single-embed card at the `writes.rs` choke point. The conversion
//! itself lives in `channels::discord::embed`; these tests pin the two halves
//! that can regress silently: which bodies are converted at all, and which
//! shapes stay plain because an embed would break them.
//!
//! A card that flips back to plain text mid-turn is the failure this file
//! guards hardest: the decision is a pure function of the body, so a create and
//! its later edits must always agree.

use crate::channels::discord::embed::{
    AUTO_EMBED_COLOR, DESCRIPTION_MAX, auto_embed_create, auto_embed_edit, builder_content,
    should_embed,
};
use serenity::builder::{CreateActionRow, CreateButton, CreateMessage, EditMessage};
use serenity::model::channel::MessageFlags;

/// Embed blocks in a serialised builder (`[]` when none were set).
fn embeds(json: &serde_json::Value) -> &Vec<serde_json::Value> {
    json["embeds"]
        .as_array()
        .unwrap_or_else(|| panic!("embeds must serialise as an array: {json}"))
}

/// One button row, the shape a pager or a suggestion menu uses.
fn buttons(custom_id: &str) -> Vec<CreateActionRow> {
    let row = CreateActionRow::Buttons(vec![CreateButton::new(custom_id)]);
    vec![row]
}

#[test]
fn empty_and_whitespace_bodies_stay_plain() {
    // Discord refuses an embed with no description, so wrapping one of these
    // would turn a harmless no-op into a 400.
    assert!(!should_embed(""));
    assert!(!should_embed("   "));
    assert!(!should_embed("\n\t  \r\n"));
}

#[test]
fn body_at_the_description_limit_still_embeds() {
    assert!(should_embed(&"d".repeat(DESCRIPTION_MAX)));
}

#[test]
fn body_past_the_description_limit_stays_plain() {
    // Clipping the tail of an answer would be worse than a long plain message.
    assert!(!should_embed(&"d".repeat(DESCRIPTION_MAX + 1)));
}

#[test]
fn builder_content_reads_the_content_field() {
    let msg = CreateMessage::new().content("hi");
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(builder_content(&json), Some("hi"));

    let empty = CreateMessage::new();
    let none = serde_json::to_value(&empty).unwrap();
    assert_eq!(builder_content(&none), None);
}

#[test]
fn content_only_create_becomes_a_card() {
    let msg = CreateMessage::new().content("hello");
    let out = auto_embed_create(msg);
    let json = serde_json::to_value(&out).unwrap();
    let blocks = embeds(&json);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["description"], serde_json::json!("hello"));
    assert_eq!(blocks[0]["color"], serde_json::json!(AUTO_EMBED_COLOR));
    assert!(
        json["content"].as_str().unwrap_or_default().is_empty(),
        "the text must move into the embed, not be duplicated above it: {json}"
    );
}

#[test]
fn over_long_create_stays_plain() {
    let body = "d".repeat(DESCRIPTION_MAX + 1);
    let msg = CreateMessage::new().content(body);
    let out = auto_embed_create(msg);
    let json = serde_json::to_value(&out).unwrap();
    assert!(embeds(&json).is_empty());
    assert_eq!(
        json["content"].as_str().map(str::len),
        Some(DESCRIPTION_MAX + 1),
        "an over-long body must ship whole as plain text: {json}"
    );
}

#[test]
fn create_with_components_still_becomes_a_card() {
    // The conversion mutates the builder instead of rebuilding it, so a button
    // row survives the wrap. A rebuild would have dropped it: serenity 0.12's
    // payload types are Serialize-only, so they cannot be read back out.
    let rows = buttons("ok");
    let msg = CreateMessage::new().content("pick one").components(rows);
    let out = auto_embed_create(msg);
    let json = serde_json::to_value(&out).unwrap();
    let blocks = embeds(&json);
    assert_eq!(blocks.len(), 1, "{json}");
    assert_eq!(blocks[0]["description"], serde_json::json!("pick one"));
    assert!(
        json.get("components").is_some(),
        "the button row must survive the wrap: {json}"
    );
}

#[test]
fn create_with_a_silent_flag_keeps_the_flag() {
    // The flag carries the suppress-notifications decision; losing it would
    // turn a quiet scheduled report into a pinging one.
    let msg = CreateMessage::new()
        .content("quiet")
        .flags(MessageFlags::SUPPRESS_NOTIFICATIONS);
    let out = auto_embed_create(msg);
    let json = serde_json::to_value(&out).unwrap();
    assert_eq!(embeds(&json).len(), 1, "{json}");
    assert!(
        json.get("flags").is_some(),
        "the silent flag must survive the wrap: {json}"
    );
}

#[test]
fn create_that_already_carries_an_embed_is_left_alone() {
    // Stacking a second card on a message that already has one would show the
    // body twice, so the caller's own embed wins.
    let existing = serenity::builder::CreateEmbed::new().title("already");
    let msg = CreateMessage::new().content("hello").add_embed(existing);
    let out = auto_embed_create(msg);
    let json = serde_json::to_value(&out).unwrap();
    assert_eq!(json["content"], serde_json::json!("hello"));
    let blocks = embeds(&json);
    assert_eq!(blocks.len(), 1, "{json}");
    assert_eq!(blocks[0]["title"], serde_json::json!("already"));
}

#[test]
fn create_with_a_poll_is_left_alone() {
    // Discord refuses a message holding both a poll and an embed, so wrapping
    // one would turn a working send into a 400.
    let answer = serenity::builder::CreatePollAnswer::new().text("a");
    let poll = serenity::builder::CreatePoll::new()
        .question("pick")
        .answers(vec![answer])
        .duration(std::time::Duration::from_secs(3600));
    let msg = CreateMessage::new().content("vote").poll(poll);
    let out = auto_embed_create(msg);
    let json = serde_json::to_value(&out).unwrap();
    assert_eq!(json["content"], serde_json::json!("vote"));
    assert!(embeds(&json).is_empty(), "{json}");
    assert!(json.get("poll").is_some(), "the poll must survive: {json}");
}

#[test]
fn create_without_content_is_untouched() {
    // An attachment-only or poll-only message has no text to move.
    let msg = CreateMessage::new();
    let out = auto_embed_create(msg);
    let json = serde_json::to_value(&out).unwrap();
    assert!(json.get("content").is_none());
    assert!(embeds(&json).is_empty());
}

#[test]
fn blank_content_create_is_untouched() {
    let msg = CreateMessage::new().content("   ");
    let out = auto_embed_create(msg);
    let json = serde_json::to_value(&out).unwrap();
    assert_eq!(json["content"], serde_json::json!("   "));
    assert!(embeds(&json).is_empty());
}

#[test]
fn content_only_edit_becomes_a_card() {
    let edit = EditMessage::new().content("hello");
    let out = auto_embed_edit(edit);
    let json = serde_json::to_value(&out).unwrap();
    let blocks = embeds(&json);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["description"], serde_json::json!("hello"));
    assert!(
        json["content"].as_str().unwrap_or_default().is_empty(),
        "the text must move into the embed, not be duplicated above it: {json}"
    );
}

#[test]
fn edit_with_components_still_becomes_a_card() {
    // The edit half mutates in place too, so a pager's button row survives the
    // wrap exactly as it does on the create path.
    let rows = buttons("next");
    let edit = EditMessage::new().content("page 2").components(rows);
    let out = auto_embed_edit(edit);
    let json = serde_json::to_value(&out).unwrap();
    let blocks = embeds(&json);
    assert_eq!(blocks.len(), 1, "{json}");
    assert_eq!(blocks[0]["description"], serde_json::json!("page 2"));
    assert!(
        json.get("components").is_some(),
        "the button row must survive the wrap: {json}"
    );
}

#[test]
fn a_card_stays_a_card_across_create_then_edit() {
    // The same body must reach the same decision on both paths, otherwise a
    // tool bubble that arrives as a card reverts to plain text on its next
    // edit, which is the exact flip this design exists to prevent.
    let cm = CreateMessage::new().content("report");
    let em = EditMessage::new().content("report");
    let cj = serde_json::to_value(auto_embed_create(cm)).unwrap();
    let ej = serde_json::to_value(auto_embed_edit(em)).unwrap();
    assert_eq!(embeds(&cj)[0]["description"], serde_json::json!("report"));
    assert_eq!(embeds(&ej)[0]["description"], serde_json::json!("report"));
}

#[test]
fn edit_with_components_still_becomes_a_card() {
    // The tool-group Expand/Collapse toggle redraws through `writes::edit`, so
    // the edit path is the one that must keep a button row alive while the text
    // moves into the embed. Losing the row would leave a card with no way back
    // to its collapsed state.
    let rows = buttons("toolgroup:7");
    let msg = EditMessage::new()
        .content("• 3 tool calls")
        .components(rows);
    let out = auto_embed_edit(msg);
    let json = serde_json::to_value(&out).unwrap();
    let blocks = embeds(&json);
    assert_eq!(blocks.len(), 1, "{json}");
    assert_eq!(
        blocks[0]["description"],
        serde_json::json!("• 3 tool calls")
    );
    assert!(
        json.get("components").is_some(),
        "the button row must survive the wrap: {json}"
    );
    assert!(
        json["content"].as_str().unwrap_or_default().is_empty(),
        "the text must move into the embed, not be duplicated above it: {json}"
    );
}

#[test]
fn edit_that_already_carries_an_embed_is_left_alone() {
    let existing = serenity::builder::CreateEmbed::new().title("already");
    let msg = EditMessage::new().content("hello").add_embed(existing);
    let out = auto_embed_edit(msg);
    let json = serde_json::to_value(&out).unwrap();
    assert_eq!(json["content"], serde_json::json!("hello"));
    let blocks = embeds(&json);
    assert_eq!(blocks.len(), 1, "{json}");
    assert_eq!(blocks[0]["title"], serde_json::json!("already"));
}

/// The wiring, read out of the source. The tests above exercise the helpers
/// directly, so a refactor that stopped calling them would leave every one of
/// them green while the feature silently did nothing.
#[test]
fn the_choke_point_applies_the_conversion() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("src/channels/discord/writes.rs");
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let flat: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        flat.contains("channels.discord.auto_embed"),
        "writes.rs must read the auto_embed flag"
    );
    assert!(
        flat.contains("auto_embed_create("),
        "writes.rs must convert creates"
    );
    assert!(
        flat.contains("auto_embed_edit("),
        "writes.rs must convert edits"
    );
    assert!(
        flat.contains("should_embed("),
        "say must ask the body predicate before taking the send_message path"
    );
    // The plain path must survive: `say` keeps a real `channel.say` call, which
    // is also what discord_write_discipline_test requires of this file.
    assert!(flat.contains(".say("), "say must keep the plain path");

    // The tool-group toggle must redraw through the SAME governed path the card
    // was created on. It used to answer with a raw interaction response, which
    // tied the redraw to this handler winning the acknowledgement race: whenever
    // something else acked first the response came back 40060 and the card was
    // never redrawn, so Expand looked dead. Pin the shape that fixes it, and pin
    // the absence of the raw-response path so it cannot come back.
    let agent_path = root.join("src/channels/discord/agent.rs");
    let agent_src = std::fs::read_to_string(&agent_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", agent_path.display()));
    let agent_flat: String = agent_src.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        agent_flat.contains("writes::edit("),
        "the tool-group toggle must redraw through the governed write path"
    );
    assert!(
        !agent_flat.contains("auto_embed_update("),
        "the toggle must not answer with a raw interaction response: that is the \
         acknowledgement race that made Expand look dead"
    );
}
