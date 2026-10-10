//! Live context budget on the Discord flow card (#1841 parity).
//!
//! Telegram stamps the `ctx: used/max pct` segment only when the turn is
//! delivered (`src/channels/telegram/flow.rs`), so on Discord the card would
//! show the budget only once it settles. The tool loop already emits a real
//! token count every iteration (`ProgressEvent::TokenCount`), so the Discord
//! handler streams that count into the live card instead. These tests pin the
//! contract: the segment renders when present, stays absent until the first
//! count, survives a re-upsert, and the handler actually wires the event in.

use crate::channels::discord::DiscordState;
use crate::channels::discord::tool_group::{
    GroupEntry, GroupState, SettledStatus, TurnOutcome, render_content,
};
use std::time::{Duration, Instant};

fn live_group() -> GroupState {
    GroupState {
        last_activity_at: Instant::now(),
        entries: vec![GroupEntry {
            name: "bash".to_string(),
            context: " (cargo test)".to_string(),
            status: None,
        }],
        expanded: false,
        notes: Vec::new(),
        started_at: Instant::now(),
        live_ctx: None,
        settled: None,
    }
}

/// The budget rides the live line, between the activity tail and the clock.
#[test]
fn live_line_renders_the_ctx_segment_when_present() {
    let mut group = live_group();
    group.live_ctx = Some("ctx: 8K/200K 4%".to_string());
    let body = render_content(&group);
    assert!(
        body.contains("ctx: 8K/200K 4%"),
        "the live card must show the ctx budget while the turn runs: {body}"
    );
}

/// Nothing is stamped before the first token count, so no empty `ctx:` husk.
#[test]
fn live_line_omits_the_ctx_segment_when_absent() {
    let body = render_content(&live_group());
    assert!(
        !body.contains("ctx:"),
        "no budget segment until a count arrives: {body}"
    );
}

/// A settled card keeps its own ctx source and must not carry the live one.
#[tokio::test]
async fn set_live_ctx_stores_the_segment_for_the_next_render() {
    let state = DiscordState::new();
    state.upsert_tool_group(222, live_group()).await;
    let updated = state
        .set_live_ctx(222, "ctx: 12K/200K 6%".to_string())
        .await
        .expect("the group is stored, so the update lands");
    assert_eq!(updated.live_ctx.as_deref(), Some("ctx: 12K/200K 6%"));
    assert!(
        render_content(&updated).contains("ctx: 12K/200K 6%"),
        "the stored segment must reach the rendered card"
    );
}

#[tokio::test]
async fn set_live_ctx_on_a_missing_group_is_none() {
    let state = DiscordState::new();
    assert!(
        state
            .set_live_ctx(404, "ctx: 1K/200K 1%".to_string())
            .await
            .is_none(),
        "a pruned or never-born group has no card to stamp"
    );
}

/// Tool events re-upsert the group constantly; the budget is not part of an
/// event, so the upsert must carry the stored one forward like `notes`.
#[tokio::test]
async fn upsert_preserves_the_live_ctx() {
    let state = DiscordState::new();
    state.upsert_tool_group(333, live_group()).await;
    state.set_live_ctx(333, "ctx: 9K/200K 5%".to_string()).await;
    let updated = state.upsert_tool_group(333, live_group()).await;
    assert_eq!(
        updated.live_ctx.as_deref(),
        Some("ctx: 9K/200K 5%"),
        "a re-upserted group must keep the budget it was showing"
    );
}

/// Lexical sentinels: the handler is the only place that can bridge the
/// agent's token count to the card, and it must go through the same
/// formatter Telegram uses, or the two channels drift.
#[test]
fn handler_streams_the_token_count_into_the_card() {
    let handler = include_str!("../channels/discord/handler.rs");
    assert_eq!(
        handler.matches("ProgressEvent::TokenCount(count)").count(),
        1,
        "exactly one live arm: the tool loop's count feeds the card"
    );
    assert!(
        handler.contains("dstate.set_live_ctx(mid.get(), ctx)"),
        "the live arm must write the segment through set_live_ctx"
    );
    assert!(
        handler.contains("crate::utils::format_ctx_footer("),
        "Discord must reuse Telegram's formatter, never a local copy"
    );
}

/// The settled line owns the budget (#1841), so a count that lands after
/// settle must not paint the live segment over the frozen one.
#[test]
fn a_settled_card_renders_the_settled_ctx_not_the_live_one() {
    let mut group = live_group();
    group.entries.push(GroupEntry {
        name: "grep".to_string(),
        context: " (ctx)".to_string(),
        status: Some(true),
    });
    group.live_ctx = Some("ctx: 1K/200K 1%".to_string());
    group.settled = Some(SettledStatus::new(
        TurnOutcome::Finished,
        Duration::from_secs(90),
        Some("ctx: 84K/200K 42%".to_string()),
    ));
    let body = render_content(&group);
    assert!(
        body.contains("ctx: 84K/200K 42%"),
        "the settled chrome keeps the budget it was frozen with: {body}"
    );
    assert!(
        !body.contains("ctx: 1K/200K 1%"),
        "a late live count must never overwrite the settled budget: {body}"
    );
}
