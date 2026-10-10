//! FR-005 (#1880): the settled flow card must be HONEST about how the turn
//! ended.
//!
//! Two defects motivated these guards, both found by reading the shipped code:
//!
//! 1. Settle ran only inside the `Ok` arm. A turn that failed, timed out or
//!    was cancelled left the card's `🕒` clock spinning forever — on screen
//!    indistinguishable from a turn still working.
//! 2. The settled icon was derived from TOOL status, so a turn that timed out
//!    or was cancelled with every tool green rendered a green check: a false
//!    success signal on the card the user is actually watching.
//!
//! The guards below pin the contract: the outcome drives the settled icon, a
//! non-Finished turn says the result may be incomplete, and the progress trace
//! is never deleted by settling.

use crate::channels::discord::tool_group::{
    GroupEntry, GroupState, SettledStatus, TurnOutcome, render_content,
};
use std::time::{Duration, Instant};

/// `n` tools, every one of them GREEN — the exact shape that used to render a
/// check on a timeout.
fn green_tools(n: usize) -> Vec<GroupEntry> {
    (0..n)
        .map(|i| GroupEntry {
            name: format!("tool{i}"),
            context: format!(" (arg{i})"),
            status: Some(true),
        })
        .collect()
}

/// A settled group with `n` green tools and the given outcome, COLLAPSED.
///
/// Collapsed matters. `render_content` then yields the settled SUMMARY line
/// alone; the per-tool trace lines are not in the body. That is deliberate: a
/// green tool line legitimately renders `✅` (see `entry_icon`), so asserting
/// "no check anywhere in the body" against an EXPANDED group fails on the
/// trace rather than on the settled line, and tests the wrong thing.
fn settled(n: usize, outcome: TurnOutcome) -> GroupState {
    GroupState {
        last_activity_at: Instant::now(),
        entries: green_tools(n),
        expanded: false,
        notes: Vec::new(),
        started_at: Instant::now(),
        live_ctx: None,
        settled: Some(SettledStatus::new(
            outcome,
            Duration::from_secs(90),
            Some("ctx: 84K/200K 42%".into()),
        )),
    }
}

#[test]
fn a_timeout_names_the_actual_tool_count_and_the_incomplete_result() {
    let body = render_content(&settled(3, TurnOutcome::TimedOut));
    assert!(
        body.contains("Timed out"),
        "the settled line must name the outcome: {body}"
    );
    assert!(
        body.contains("**3 tool calls**"),
        "the settled line must carry the ACTUAL tool count: {body}"
    );
    assert!(
        body.contains("result may be incomplete"),
        "AC-010: a turn that stopped early must say so: {body}"
    );
}

#[test]
fn a_timeout_with_all_green_tools_never_renders_a_check() {
    // The regression: three green tools + a timeout used to render "✅" as the
    // settled signal, because the icon came from tool status, not the outcome.
    let body = render_content(&settled(3, TurnOutcome::TimedOut));
    assert!(
        !body.contains('✅'),
        "a timed-out turn must never show a success check: {body}"
    );
    assert!(
        body.contains('⏱'),
        "the timeout glyph is the honest signal here: {body}"
    );
}

#[test]
fn a_cancelled_turn_never_renders_a_check() {
    let body = render_content(&settled(2, TurnOutcome::Cancelled));
    assert!(
        !body.contains('✅'),
        "cancelled must not read as success: {body}"
    );
    assert!(body.contains("Cancelled"), "{body}");
    assert!(body.contains("result may be incomplete"), "{body}");
}

#[test]
fn a_failed_turn_says_failed_and_keeps_the_failure_count() {
    let mut g = settled(2, TurnOutcome::Failed);
    g.entries[0].status = Some(false);
    let body = render_content(&g);
    assert!(body.contains("Failed"), "{body}");
    assert!(!body.contains('✅'), "{body}");
    assert!(
        body.contains("1 failed"),
        "the per-tool failure count stays as supporting detail: {body}"
    );
}

#[test]
fn a_finished_turn_still_reads_as_success() {
    let body = render_content(&settled(3, TurnOutcome::Finished));
    assert!(body.contains('✅'), "{body}");
    assert!(body.contains("Finished"), "{body}");
    assert!(
        !body.contains("result may be incomplete"),
        "a clean finish must NOT hedge: {body}"
    );
}

#[test]
fn the_progress_trace_survives_the_settle() {
    // AC-011: settling is a re-render, never a delete. Expanded so the per-tool
    // trace lines and the narration note are part of the rendered body too.
    let mut g = settled(3, TurnOutcome::TimedOut);
    g.expanded = true;
    g.notes = vec!["Scanning the repository".to_string()];
    let body = render_content(&g);
    assert!(body.contains("tool0") && body.contains("tool2"), "{body}");
    assert!(
        body.contains("Scanning the repository"),
        "the narration note must survive settling: {body}"
    );
    assert!(
        !body.contains('🕒'),
        "the live clock is replaced by the frozen one: {body}"
    );
    assert!(
        body.contains("1:30"),
        "the elapsed clock is frozen at settle time: {body}"
    );
}

#[test]
fn classify_outcome_splits_timeouts_from_generic_failures() {
    use crate::brain::agent::AgentError;
    use crate::channels::discord::handler::classify_outcome;

    for msg in ["request timed out", "upstream TIMEOUT", "deadline exceeded"] {
        assert_eq!(
            classify_outcome(&AgentError::ToolError(msg.to_string())),
            TurnOutcome::TimedOut,
            "{msg} must classify as a timeout"
        );
    }
    assert_eq!(
        classify_outcome(&AgentError::ToolError("boom".to_string())),
        TurnOutcome::Failed,
    );
    assert_eq!(
        classify_outcome(&AgentError::ContextTooLarge {
            current: 10,
            limit: 5
        }),
        TurnOutcome::Failed,
    );
}
