//! The flow ticker's decision surface (#1807): Slack's clock must re-render
//! on a throttle, not freeze between tool events. The loop itself is a
//! network task (chat.update against a live session), so the tests pin the
//! snapshot contract it decides on, plus a lexical sentinel for the
//! rate-limit throttle: 4 s, never Telegram's 1500 ms.

use crate::channels::slack::SlackState;
use crate::channels::slack::tool_group::{GroupState, TurnOutcome};
use slack_morphism::prelude::SlackChannelId;

fn live_group() -> GroupState {
    GroupState::new(SlackChannelId::new("C123".into()), vec![])
}

#[tokio::test]
async fn snapshot_reports_a_live_group_so_the_ticker_ticks() {
    let state = SlackState::new();
    state.upsert_tool_group("111.0".into(), live_group()).await;
    let snap = state.tool_group_snapshot("111.0").await;
    assert!(snap.is_some(), "live group must be visible to the ticker");
    assert!(
        snap.unwrap().settled.is_none(),
        "group is live: the ticker must keep ticking"
    );
}

#[tokio::test]
async fn snapshot_reports_settled_so_the_ticker_stops() {
    let state = SlackState::new();
    state.upsert_tool_group("111.0".into(), live_group()).await;
    state
        .settle_tool_group("111.0", TurnOutcome::Finished, None, None)
        .await;
    let snap = state
        .tool_group_snapshot("111.0")
        .await
        .expect("settled group is still stored");
    assert!(
        snap.settled_terminal(),
        "terminal settle must stay visible to the ticker: it is the stop condition (#1807, #1988)"
    );
}

#[tokio::test]
async fn waiting_settle_does_not_stop_the_ticker() {
    // #1988: the flip is what ends the clock, not the waiting settle. A
    // group waiting on background work must keep rolling its 🕒.
    let state = SlackState::new();
    state.upsert_tool_group("111.0".into(), live_group()).await;
    state
        .settle_tool_group(
            "111.0",
            TurnOutcome::Finished,
            Some("Waiting for 1 background task".into()),
            None,
        )
        .await;
    let snap = state
        .tool_group_snapshot("111.0")
        .await
        .expect("waiting group is still stored");
    assert!(
        !snap.settled_terminal(),
        "a waiting line keeps the clock rolling until the flip"
    );
    // The terminal flip is what stops it.
    state
        .settle_tool_group("111.0", TurnOutcome::Finished, None, None)
        .await;
    let snap = state
        .tool_group_snapshot("111.0")
        .await
        .expect("flipped group is still stored");
    assert!(snap.settled_terminal(), "terminal settle stops the ticker");
}

#[tokio::test]
async fn snapshot_missing_group_is_none() {
    let state = SlackState::new();
    assert!(
        state.tool_group_snapshot("404.0").await.is_none(),
        "a pruned or never-born group stops the ticker via None"
    );
}

/// The throttle is the rate-limit contract: Slack `chat.update` tolerates
/// roughly one write per second, so the tick must stay at 4 s. Telegram
/// runs 1500 ms; that number must never migrate here.
#[test]
fn ticker_throttle_is_rate_limit_safe() {
    let handler = include_str!("../channels/slack/handler.rs");
    assert!(
        handler.contains(
            "const FLOW_TICKER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(4);"
        ),
        "flow ticker interval must stay 4 s: chat.update rate limits (#1807)"
    );
    assert!(
        handler.contains(
            "const FLOW_TICKER_CAP: std::time::Duration = std::time::Duration::from_secs(30 * 60);"
        ),
        "flow ticker must keep its 30 min safety cap (#1807)"
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
    let handler = include_str!("../channels/slack/handler.rs");
    let start = handler
        .find("let Some(group) = slack_state.tool_group_snapshot(ts.as_ref()).await")
        .expect("the ticker no longer reads the group snapshot");
    let rest = &handler[start..];
    let end = rest
        .find("if group.settled_terminal()")
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
    // Bound the helper to its own body: the ticker above it calls `update` too,
    // so an unbounded slice would satisfy the class assertion by accident.
    let helper = &handler[helper_start..];
    let helper_end = helper[1..]
        .find("\nasync fn ")
        .map(|i| i + 1)
        .unwrap_or(helper.len());
    let helper = &helper[..helper_end];
    assert!(
        helper.contains("group.settle("),
        "the orphan helper no longer settles the group, so a lost card would \
         keep live chrome"
    );
    assert!(
        helper.contains("session.update(&upd)"),
        "the orphan repair must not be droppable: a rate-limited drop leaves the \
         frozen clock the helper exists to fix"
    );
}
