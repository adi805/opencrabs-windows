//! Discord settled-card parity with Telegram (#1144/#1183): a turn that
//! FINISHED while detached background tasks or sub-agents are still alive must
//! settle to `⏳ Waiting for …`, never `✅ Finished`.
//!
//! Telegram fixed this in two rounds. #1144 folded the alive background-task
//! count into the settled header, because a card that ended with detached
//! shell tasks read "Finished" up top and "N tasks running" in the footer. #1183
//! extended it to sub-agents, which live in a separate registry the
//! background-task count never read. Discord renders the same chrome from its
//! own group state, so it needs the same override and the same pair of
//! registries — this file pins both, plus the single-tool card that used to
//! drop the settled line entirely.
//!
//! The settle sites themselves are network calls against a live session, so
//! the registries are read through one shared helper (`DiscordState::
//! waiting_counts`) and the rendering is exercised directly.

use crate::channels::background_work::{SubagentCounts, subagent_waiting_phrase};
use crate::channels::discord::DiscordState;
use crate::channels::discord::tool_group::{
    GroupEntry, GroupState, TurnOutcome, render_content, settled_icon_verb,
};
use std::time::Instant;

fn group(n: usize) -> GroupState {
    GroupState {
        last_activity_at: Instant::now(),
        entries: (0..n)
            .map(|i| GroupEntry {
                name: format!("tool{i}"),
                context: format!(" (arg{i})"),
                status: Some(true),
            })
            .collect(),
        expanded: false,
        notes: Vec::new(),
        started_at: Instant::now(),
        live_ctx: None,
        settled: None,
    }
}

/// The alive-agent phrase for a given split, the way the settle sites build it.
fn phrase(working: usize, awaiting: usize) -> String {
    subagent_waiting_phrase(SubagentCounts { working, awaiting })
}

// --- The pure override ---------------------------------------------------

#[test]
fn a_clean_finish_keeps_the_check() {
    let (icon, verb) = settled_icon_verb(TurnOutcome::Finished, 0, None);
    assert_eq!(icon, "✅");
    assert_eq!(verb, "Finished");
}

#[test]
fn one_background_task_overrides_finished() {
    let (icon, verb) = settled_icon_verb(TurnOutcome::Finished, 1, None);
    assert_eq!(icon, "⏳");
    assert_eq!(verb, "Waiting for 1 background task");
}

#[test]
fn several_background_tasks_are_pluralized() {
    let (_, verb) = settled_icon_verb(TurnOutcome::Finished, 3, None);
    assert_eq!(verb, "Waiting for 3 background tasks");
}

#[test]
fn alive_subagents_alone_override_finished() {
    let alive = phrase(2, 0);
    let (icon, verb) = settled_icon_verb(TurnOutcome::Finished, 0, Some(alive.as_str()));
    assert_eq!(icon, "⏳");
    assert_eq!(verb, "Waiting for 2 working agents");
}

#[test]
fn both_registries_join_with_a_plus() {
    let alive = phrase(2, 1);
    let (_, verb) = settled_icon_verb(TurnOutcome::Finished, 1, Some(alive.as_str()));
    let expected = "Waiting for 1 background task + 3 agents (2 working, 1 awaiting collection)";
    assert_eq!(verb, expected);
}

#[test]
fn a_parked_agent_reads_as_awaiting_collection() {
    let alive = phrase(0, 1);
    let (_, verb) = settled_icon_verb(TurnOutcome::Finished, 0, Some(alive.as_str()));
    assert_eq!(verb, "Waiting for 1 agent awaiting collection");
}

#[test]
fn a_failed_turn_keeps_its_terminal_verb_despite_alive_work() {
    // Only `Finished` is overridden. A failed turn still holds its detached
    // work, but "Waiting for" there would hide that the turn broke.
    let alive = phrase(1, 0);
    let (icon, verb) = settled_icon_verb(TurnOutcome::Failed, 2, Some(alive.as_str()));
    assert_eq!(icon, "❌");
    assert_eq!(verb, "Failed");

    let (icon, verb) = settled_icon_verb(TurnOutcome::TimedOut, 1, None);
    assert_eq!(icon, "⏱");
    assert_eq!(verb, "Timed out");

    let (icon, verb) = settled_icon_verb(TurnOutcome::Cancelled, 1, None);
    assert_eq!(icon, "❌");
    assert_eq!(verb, "Cancelled");
}

// --- The rendered card ---------------------------------------------------

#[tokio::test]
async fn a_settle_with_alive_background_work_renders_the_waiting_pair() {
    let state = DiscordState::new();
    state.upsert_tool_group(7, group(2)).await;
    let stamped = state
        .settle_tool_group(
            7,
            TurnOutcome::Finished,
            1,
            SubagentCounts::default(),
            Some("ctx: 8K/200K 4% | 45 tok/s".into()),
        )
        .await
        .expect("group exists");
    let body = render_content(&stamped);
    assert!(
        body.contains("⏳ Waiting for 1 background task"),
        "a finished turn holding detached work must say so: {body}"
    );
    assert!(
        !body.contains("✅ Finished"),
        "the false finish signal came back: {body}"
    );
    assert!(
        body.contains("ctx: 8K/200K 4% | 45 tok/s"),
        "the budget line must ride the settled chrome: {body}"
    );
}

#[tokio::test]
async fn a_settle_with_alive_subagents_renders_the_waiting_pair() {
    let state = DiscordState::new();
    state.upsert_tool_group(8, group(2)).await;
    let stamped = state
        .settle_tool_group(
            8,
            TurnOutcome::Finished,
            0,
            SubagentCounts {
                working: 1,
                awaiting: 0,
            },
            None,
        )
        .await
        .expect("group exists");
    let body = render_content(&stamped);
    assert!(
        body.contains("⏳ Waiting for 1 working agent"),
        "alive sub-agents live in their own registry and gate the header too: {body}"
    );
}

#[tokio::test]
async fn a_clean_settle_still_reads_finished() {
    let state = DiscordState::new();
    state.upsert_tool_group(9, group(2)).await;
    let stamped = state
        .settle_tool_group(
            9,
            TurnOutcome::Finished,
            0,
            SubagentCounts::default(),
            Some("ctx: 1K/2K 50%".into()),
        )
        .await
        .expect("group exists");
    let body = render_content(&stamped);
    assert!(body.contains("✅ Finished"), "{body}");
    assert!(!body.contains('⏳'), "nothing is waiting here: {body}");
    assert!(body.contains("ctx: 1K/2K 50%"), "{body}");
}

#[tokio::test]
async fn a_single_tool_card_shows_the_settled_chrome_after_settle() {
    let state = DiscordState::new();
    state.upsert_tool_group(11, group(1)).await;

    // Live: the lone row IS the body, no counts line (unchanged).
    let live = state.tool_group_snapshot(11).await.expect("live group");
    assert!(live.settled.is_none());
    let live_body = render_content(&live);
    assert!(live_body.contains("**tool0**"), "{live_body}");
    assert!(
        !live_body.contains("tool call"),
        "a live single-tool card stays a bare row: {live_body}"
    );

    let stamped = state
        .settle_tool_group(
            11,
            TurnOutcome::Finished,
            2,
            SubagentCounts::default(),
            Some("ctx: 4K/200K 2%".into()),
        )
        .await
        .expect("group exists");
    let body = render_content(&stamped);
    assert!(
        body.contains("⏳ Waiting for 2 background tasks"),
        "the settled chrome was dropped by the single-entry branch: {body}"
    );
    assert!(
        body.contains("**tool0**"),
        "the lone row must survive: {body}"
    );
    assert!(body.contains("ctx: 4K/200K 2%"), "{body}");
}

#[tokio::test]
async fn resettling_a_waiting_card_keeps_the_stamped_budget() {
    let state = DiscordState::new();
    state.upsert_tool_group(12, group(2)).await;
    state
        .settle_tool_group(
            12,
            TurnOutcome::Finished,
            1,
            SubagentCounts::default(),
            Some("ctx: 9K/200K 4%".into()),
        )
        .await;
    // A later re-settle with no ctx (e.g. a second settle path) must not wipe
    // the budget, and must not flip the waiting pair back to Finished.
    let again = state
        .settle_tool_group(
            12,
            TurnOutcome::Finished,
            1,
            SubagentCounts::default(),
            None,
        )
        .await
        .expect("group exists");
    let body = render_content(&again);
    assert!(body.contains("ctx: 9K/200K 4%"), "{body}");
    assert!(body.contains("⏳ Waiting for 1 background task"), "{body}");
}
