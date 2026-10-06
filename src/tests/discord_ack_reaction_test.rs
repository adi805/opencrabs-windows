//! FR-010 / AC-022: a receipt reaction on the user's message.
//!
//! The claim is about ORDER (the ack lands before the first progress write) and
//! about the ack being best-effort. Both are structural properties of
//! `handle_message`, so they are pinned structurally rather than by timing a
//! live turn:
//!
//! - AC-022: the 👀 reaction fires before the first `upsert_tool_group`, so the
//!   user sees the message was picked up before the progress card appears.
//! - The ack must never become a hard gate: it is skipped while the channel is
//!   backing off from a 429, because a dropped reaction is transient while the
//!   reply itself still has to be delivered.

use std::path::Path;

/// Flattened source: all whitespace removed, so a call split across lines still
/// matches a single-line needle.
fn flattened(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

const ACK: &str = "ReactionType::Unicode(\"\u{1F440}\".to_string())";

/// AC-022: the receipt reaction is on the USER's message and precedes the
/// first progress write of the turn.
#[test]
fn the_ack_reaction_fires_before_the_first_progress_write() {
    let flat = flattened("src/channels/discord/handler.rs");

    let ack = flat
        .find(ACK)
        .unwrap_or_else(|| panic!("AC-022: the receipt reaction is gone from handle_message"));
    let card = flat
        .find("upsert_tool_group(")
        .expect("the progress card must still be written from handle_message");

    assert!(
        ack < card,
        "AC-022: the ack must fire BEFORE the first progress write, found the \
         card at {card} and the ack at {ack}"
    );

    // The reaction rides on the incoming message, not on a channel write, so
    // the ack cannot be mistaken for the reply itself.
    assert!(
        flat.contains(&format!("msg.react(&ctx.http,{ACK})")),
        "AC-022: the ack must react to the incoming message (`msg.react(...)`)"
    );
}

/// The ack is best-effort: it is gated on the governor's cooldown so a channel
/// backing off from a 429 skips the reaction instead of failing the turn.
#[test]
fn the_ack_is_skipped_while_the_channel_backs_off() {
    let flat = flattened("src/channels/discord/handler.rs");

    let gate = flat
        .find("cooldown_remaining(msg.channel_id.get()).is_none()")
        .expect("the ack must be gated on the governor cooldown");

    let ack = flat.find(ACK).expect("the receipt reaction is gone");

    assert!(
        gate < ack,
        "the cooldown gate must wrap the ack, found the gate at {gate} and the \
         ack at {ack}"
    );
}
