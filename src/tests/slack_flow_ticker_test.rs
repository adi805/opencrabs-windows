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
