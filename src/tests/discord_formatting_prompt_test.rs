//! Discord formatting preamble.
//!
//! Discord has no table markup, so a pipe table reaches the channel as its own
//! source in a proportional font where nothing lines up. `table_convert`
//! repairs that for text the channel sends itself, but the cheapest fix is a
//! table the model never writes: the channel preamble has to say so.
//!
//! Slack has carried that instruction since #1016 (`SLACK_FORMATTING`);
//! Discord's preamble said nothing about formatting at all, which is how a
//! three-column table reached a phone screen as raw pipes. These tests pin the
//! instruction, plus the two wiring facts that can rot independently: the
//! handler must route through `discord_preamble`, and the outbound tool must
//! convert before it sends.

use crate::channels::discord::formatting_prompt::{DISCORD_FORMATTING, discord_preamble};
use crate::channels::discord::table_convert::tables_to_discord;

const HANDLER: &str = include_str!("../channels/discord/handler.rs");
const TOOL: &str = include_str!("../brain/tools/discord_send.rs");

#[test]
fn the_preamble_forbids_markdown_tables() {
    assert!(
        DISCORD_FORMATTING.contains("NEVER use markdown tables"),
        "the instruction has to name the construct, or the model keeps emitting it"
    );
    assert!(
        DISCORD_FORMATTING.contains("no table syntax"),
        "say WHY, so a later reader does not restore tables thinking they render"
    );
}

#[test]
fn the_preamble_names_constructs_discord_actually_renders() {
    // Bold, headings and code are what the renderer honours. Naming them keeps
    // the prompt and the renderer in agreement instead of the model guessing.
    assert!(DISCORD_FORMATTING.contains("**double asterisks**"));
    assert!(DISCORD_FORMATTING.contains("###"));
    assert!(DISCORD_FORMATTING.contains("backticks"));
}

#[test]
fn the_preamble_carries_the_channel_id() {
    let out = discord_preamble(1557147429516742747);
    assert!(out.contains("channel_id: 1557147429516742747"));
    assert!(out.contains("Do NOT call discord_send"));
}

#[test]
fn the_handler_routes_through_the_shared_preamble() {
    // The pre-fix shape: the preamble hand-rolled inline in handler.rs, with
    // no formatting guidance and no single place to change it.
    assert!(
        HANDLER.contains("formatting_prompt::discord_preamble("),
        "the handler must build its preamble from formatting_prompt"
    );
    assert!(
        !HANDLER.contains("[Channel: Discord (channel_id: {channel_id})"),
        "the hand-rolled preamble is back; the formatting rules will drift from it"
    );
}

#[test]
fn the_outbound_tool_converts_tables_before_sending() {
    // `discord_send` bypassed `tables_to_discord`, so a table the agent sent
    // through the tool reached the channel raw even though the channel's own
    // reply path would have converted it.
    assert!(
        TOOL.contains("table_convert::tables_to_discord("),
        "discord_send must convert tables, not just the automatic reply path"
    );
    assert!(
        TOOL.matches("table_convert::tables_to_discord(").count() >= 3,
        "send, reply and edit each need the conversion"
    );
}

#[test]
fn conversion_is_what_makes_a_table_readable() {
    // The behaviour the instruction exists to avoid needing: given a table
    // anyway, the channel converts it to a monospace grid rather than shipping
    // raw pipes.
    let table = "| File | Delta |\n|---|---|\n| a.rs | +3 |";
    let out = tables_to_discord(table);
    assert!(out.starts_with("```text"));
    assert!(!out.contains('|'));
}
