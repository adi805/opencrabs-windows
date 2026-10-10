//! Tests for the collapsible Discord tool group (#380): render modes,
//! toggle/preservation semantics, and retention pruning — mirroring the
//! Slack port's contracts.

use crate::channels::discord::DiscordState;
use crate::channels::discord::tool_group::{
    GroupEntry, GroupState, SettledStatus, TurnOutcome, render_components, render_content,
};
use crate::channels::telegram::flow::SubagentCounts;
use std::time::{Duration, Instant};

fn entries(n: usize, done: bool) -> Vec<GroupEntry> {
    (0..n)
        .map(|i| GroupEntry {
            name: format!("tool{i}"),
            context: format!(" (arg{i})"),
            status: if done { Some(true) } else { None },
        })
        .collect()
}

fn group(n: usize, done: bool, expanded: bool) -> GroupState {
    GroupState {
        last_activity_at: Instant::now(),
        entries: entries(n, done),
        expanded,
        notes: Vec::new(),
        started_at: Instant::now(),
        live_ctx: None,
        settled: None,
    }
}

#[test]
fn collapsed_shows_summary_expanded_lists_tools() {
    let collapsed = render_content(&group(3, false, false));
    assert!(collapsed.contains("3 tool calls"));
    assert!(collapsed.contains("running"));
    assert!(!collapsed.contains("tool0"));

    let expanded = render_content(&group(3, true, true));
    assert!(expanded.contains("tool0") && expanded.contains("tool2"));

    let single = render_content(&group(1, false, false));
    assert!(single.contains("tool0"));
    assert!(!single.contains("tool call"));
}

#[test]
fn toggle_button_appears_with_something_to_reveal() {
    // One lone tool row and nothing else: expanding would show the same line
    // the summary already carries, so there is no button.
    assert!(render_components(&group(1, false, false), 7).is_empty());
    assert_eq!(render_components(&group(2, false, false), 7).len(), 1);

    // Narration counts as something to reveal (#1990): notes are
    // expansion-only, so without the button the transcript is unreachable.
    let mut narrated = group(1, false, false);
    narrated.notes = vec!["Scanning the repository".to_string()];
    assert_eq!(
        render_components(&narrated, 7).len(),
        1,
        "a narration-only card must still offer Expand"
    );
}

#[test]
fn narration_rows_are_expansion_only() {
    let mut g = group(3, false, false);
    g.notes = vec![
        "Reading the old renderer".to_string(),
        "Scanning the repository".to_string(),
    ];

    let collapsed = render_content(&g);
    assert!(
        !collapsed.contains("-#"),
        "collapsed card ends at the summary line, no narration rows: {collapsed}"
    );
    // The newest note still leads as the live activity — hiding the rows is
    // not hiding the signal.
    assert!(
        collapsed.contains("Scanning the repository"),
        "the latest note stays as the summary activity: {collapsed}"
    );

    g.expanded = true;
    let expanded = render_content(&g);
    assert!(
        expanded.contains("-# Reading the old renderer")
            && expanded.contains("-# Scanning the repository"),
        "expanding reveals every narration row: {expanded}"
    );
    assert!(
        expanded.contains("3 tool calls"),
        "the summary line survives the expansion: {expanded}"
    );
}

#[tokio::test]
async fn toggle_flips_and_updates_preserve_expansion() {
    let state = DiscordState::new();
    state.upsert_tool_group(111, group(2, false, false)).await;
    let toggled = state.toggle_tool_group(111).await.expect("exists");
    assert!(toggled.expanded);
    // Progress updates (built collapsed) must preserve the user's choice.
    let stored = state.upsert_tool_group(111, group(2, true, false)).await;
    assert!(stored.expanded);
    assert!(state.toggle_tool_group(999).await.is_none());
}

#[tokio::test]
async fn retention_prunes_oldest_groups() {
    let state = DiscordState::new();
    for i in 0..25u64 {
        state.upsert_tool_group(i, group(2, true, false)).await;
    }
    assert!(state.toggle_tool_group(0).await.is_none());
    assert!(state.toggle_tool_group(24).await.is_some());
}

#[test]
fn live_summary_carries_the_rolling_clock_settled_freezes_it() {
    let live = render_content(&group(3, false, false));
    assert!(live.contains("🕒"));

    let mut done_group = group(2, true, false);
    done_group.settled = Some(SettledStatus::new(
        TurnOutcome::Finished,
        Duration::from_secs(90),
        Some("ctx: 84K/200K 42%".into()),
    ));
    let settled = render_content(&done_group);
    assert!(settled.contains("⏱️ 1:30"));
    assert!(settled.contains("ctx: 84K/200K 42%"));
    assert!(!settled.contains("🕒"));
}

#[tokio::test]
async fn settle_freezes_elapsed_and_stamps_ctx() {
    let state = DiscordState::new();
    let mut g = group(2, true, false);
    g.started_at = Instant::now() - Duration::from_secs(90);
    state.upsert_tool_group(77, g).await;
    let stamped = state
        .settle_tool_group(
            77,
            TurnOutcome::Finished,
            0,
            SubagentCounts::default(),
            Some("ctx: 1K/2K 50%".into()),
        )
        .await
        .expect("group exists");
    let s = stamped.settled.as_ref().expect("stamped at settle");
    assert_eq!(s.elapsed.as_secs(), 90);
    let done = render_content(&stamped);
    assert!(done.contains("ctx: 1K/2K 50%"));
    assert!(done.contains("⏱️ 1:30"));
}

#[tokio::test]
async fn upsert_preserves_started_at_and_settled() {
    let state = DiscordState::new();
    let mut g = group(1, false, false);
    g.started_at = Instant::now() - Duration::from_secs(30);
    state.upsert_tool_group(88, g).await;
    state
        .settle_tool_group(
            88,
            TurnOutcome::Finished,
            0,
            SubagentCounts::default(),
            Some("ctx: A".into()),
        )
        .await;
    // A late progress update must not restart the clock or clear the stamp.
    let stored = state.upsert_tool_group(88, group(1, true, false)).await;
    assert!(stored.started_at.elapsed().as_secs() >= 29);
    assert!(stored.settled.is_some());
}

#[tokio::test]
async fn resettle_with_no_ctx_keeps_the_stamped_budget() {
    let state = DiscordState::new();
    state.upsert_tool_group(99, group(1, true, false)).await;
    state
        .settle_tool_group(
            99,
            TurnOutcome::Finished,
            0,
            SubagentCounts::default(),
            Some("ctx: B".into()),
        )
        .await;
    let again = state
        .settle_tool_group(
            99,
            TurnOutcome::Finished,
            0,
            SubagentCounts::default(),
            None,
        )
        .await
        .expect("group exists");
    assert_eq!(
        again
            .settled
            .as_ref()
            .expect("still stamped")
            .ctx
            .as_deref(),
        Some("ctx: B")
    );
}
