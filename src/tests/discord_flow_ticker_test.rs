//! The Discord flow ticker's decision surface (#1843): the clock must
//! re-render on a throttle, not freeze between tool events. The loop itself
//! is a network task (message edits against a live session), so the tests
//! pin the snapshot contract it decides on, plus lexical sentinels for the
//! throttle (4 s, never Telegram's 1500 ms), the cap, and the spawn site.

use crate::channels::discord::DiscordState;
use crate::channels::discord::tool_group::{
    GroupEntry, GroupState, TurnOutcome, settled_icon_verb,
};
use std::time::Instant;

fn live_group() -> GroupState {
    GroupState {
        last_activity_at: Instant::now(),
        entries: (0..2)
            .map(|i| GroupEntry {
                name: format!("tool{i}"),
                context: format!(" (arg{i})"),
                status: None,
            })
            .collect(),
        expanded: false,
        notes: Vec::new(),
        started_at: Instant::now(),
        live_ctx: None,
        settled: None,
    }
}

#[tokio::test]
async fn snapshot_reports_a_live_group_so_the_ticker_ticks() {
    let state = DiscordState::new();
    state.upsert_tool_group(111, live_group()).await;
    let snap = state.tool_group_snapshot(111).await;
    assert!(snap.is_some(), "live group must be visible to the ticker");
    assert!(
        snap.unwrap().settled.is_none(),
        "group is live: the ticker must keep ticking"
    );
}

#[tokio::test]
async fn snapshot_reports_settled_so_the_ticker_stops() {
    let state = DiscordState::new();
    state.upsert_tool_group(111, live_group()).await;
    state
        .settle_tool_group(
            111,
            crate::channels::discord::tool_group::TurnOutcome::Finished,
            0,
            crate::channels::telegram::flow::SubagentCounts::default(),
            None,
        )
        .await;
    let snap = state
        .tool_group_snapshot(111)
        .await
        .expect("settled group is still stored");
    assert!(
        snap.settled.is_some(),
        "settled must stay visible to the ticker: it is the stop condition (#1843)"
    );
}

#[tokio::test]
async fn snapshot_missing_group_is_none() {
    let state = DiscordState::new();
    assert!(
        state.tool_group_snapshot(404).await.is_none(),
        "a pruned or never-born group stops the ticker via None"
    );
}

/// The throttle is the rate-limit posture: Discord's docs do not publish a
/// fixed per-route edit budget and explicitly forbid hardcoding one, and
/// serenity's built-in ratelimiter is the safety layer. The 4 s interval is
/// ours, chosen for parity with the Slack ticker (#1807). Telegram runs
/// 1500 ms; that number must never migrate here.
#[test]
fn ticker_throttle_is_rate_limit_safe() {
    let handler = include_str!("../channels/discord/handler.rs");
    assert!(
        handler.contains(
            "const FLOW_TICKER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(4);"
        ),
        "the ticker interval is 4 s, parity with Slack (#1807)"
    );
    assert!(
        !handler.contains("from_millis(1500"),
        "Telegram's 1500 ms tick must never migrate to Discord"
    );
    assert!(
        handler.contains(
            "const FLOW_TICKER_CAP: std::time::Duration = std::time::Duration::from_secs(30 * 60);"
        ),
        "orphaned ticks die at the 30 min cap: no immortal tasks"
    );
    assert!(
        handler.contains("should not be hard coded"),
        "the docs citation must stay: the budget is serenity's limiter, not a magic number"
    );
}

/// The ticker must spawn before the turn is dispatched. Since #1845 the
/// group is born at turn start (before the spawn), so the ticker finds it
/// immediately; the spawn still precedes the agent turn itself.
#[test]
fn ticker_spawns_before_the_turn_dispatch() {
    let handler = include_str!("../channels/discord/handler.rs");
    let spawn = handler
        .find("Spawns after the turn-start")
        .expect("spawn site comment present");
    let dispatch = handler
        .find("send_message_with_tools_and_display")
        .expect("turn dispatch present");
    assert!(
        spawn < dispatch,
        "spawn_flow_ticker must run before the agent turn starts"
    );
}

/// A snapshot that comes back empty must settle the card, not walk away from
/// it. The ticker's bare `break` left the live `🕒` chrome frozen on screen,
/// which reads exactly like a turn still working (#1880). The arm now hands the
/// last rendering it read to `settle_orphaned_card`, and that helper has to post
/// it with the final class: a droppable class would let the repair be
/// rate-limited away and leave the same frozen clock behind.
#[test]
fn a_missing_snapshot_settles_the_card_instead_of_bare_breaking() {
    let handler = include_str!("../channels/discord/handler.rs");
    let start = handler
        .find("let Some(group) = dstate.tool_group_snapshot(mid.get()).await")
        .expect("the ticker no longer reads the group snapshot");
    let rest = &handler[start..];
    let end = rest
        .find("if group.settled.is_some()")
        .expect("the settled check that follows the snapshot moved");
    let arm = &rest[..end];
    assert!(
        arm.contains("settle_orphaned_card"),
        "the missing-snapshot arm no longer settles the card it lost, so a card \
         whose group left the store keeps its live clock"
    );
    assert!(
        arm.contains("break"),
        "the missing-snapshot arm must still stop the ticker"
    );

    let helper_start = handler
        .find("async fn settle_orphaned_card(")
        .expect("the orphan settle helper is gone");
    let helper = &handler[helper_start..];
    assert!(
        helper.contains("stamp_settled("),
        "the orphan helper no longer stamps a settled status, so a lost card \
         would keep live chrome"
    );
    assert!(
        helper.contains("Class::Final"),
        "the orphan repair must not be droppable: a rate-limited drop leaves the \
         frozen clock the helper exists to fix"
    );
    assert!(
        helper.contains("components(Vec::new())"),
        "with no rendering to post, the helper must at least drop the toggle row \
         instead of leaving a control that can never work"
    );
}

/// #196: the store-miss repair has no cause to report. On that branch the group
/// left the store while the turn may still be running, so the card must not
/// print a timeout verdict — that tells the reader the turn died when it did
/// not. Pinned through `settled_icon_verb`, the same function `stamp_settled`
/// renders with, so this asserts the line the user reads and not the call.
#[test]
fn an_orphaned_settle_does_not_claim_a_timeout() {
    let (icon, verb) = settled_icon_verb(TurnOutcome::Orphaned, 0, None);
    assert_ne!(
        verb, "Timed out",
        "the store-miss path has no timeout to report: the turn may still be running (#196)"
    );
    assert_eq!(verb, "Orphaned");
    assert_eq!(icon, "⚠");
}

/// The ticker-cap path keeps the real timeout. A 30 min cap against a turn
/// budget measured in minutes means the turn is long gone by the time it fires,
/// so that verdict is honest and must not be swept up by the #196 change.
#[test]
fn the_ticker_cap_still_reports_a_timeout() {
    let handler = include_str!("../channels/discord/handler.rs");
    let cap = handler
        .find("if born.elapsed() > FLOW_TICKER_CAP {")
        .expect("the ticker cap arm moved");
    let rest = &handler[cap..];
    let end = rest
        .find("// Plan card (FR-008)")
        .expect("the plan-card refresh that follows the cap moved");
    let arm = &rest[..end];
    assert!(
        arm.contains("TurnOutcome::TimedOut"),
        "the cap is an honest timeout and must keep saying so (#196)"
    );
}

/// The helper must never stamp a timeout, whatever its caller passes: both call
/// sites are the same situation. With nothing to render there is no verdict to
/// post at all, so the empty arm has to return before the stamp — Discord drops
/// the toggle row, Slack has no components to drop and can only warn.
#[test]
fn the_orphan_helper_stamps_a_verdict_that_is_not_a_timeout() {
    let handler = include_str!("../channels/discord/handler.rs");
    let helper_start = handler
        .find("async fn settle_orphaned_card(")
        .expect("the orphan settle helper is gone");
    let helper = &handler[helper_start..];
    let helper_end = helper
        .find("\n}\n")
        .expect("the helper's closing brace moved");
    let helper = &helper[..helper_end];
    assert!(
        !helper.contains("TurnOutcome::TimedOut"),
        "the store-miss repair must not claim a timeout: the turn may still be running (#196)"
    );
    assert!(
        helper.contains("TurnOutcome::Orphaned"),
        "the store-miss repair must stamp the cause-free verdict (#196)"
    );
    let none_arm = &helper[..helper
        .find("return;")
        .expect("the empty-rendering arm must return early")];
    assert!(
        !none_arm.contains("stamp_settled("),
        "with no rendering to post the helper must not stamp a verdict (#196)"
    );
}
