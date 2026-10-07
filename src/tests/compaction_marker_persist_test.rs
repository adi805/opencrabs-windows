//! #1928 — the delta-segment sentinel must reach the DB.
//!
//! Both halves of this seam were individually correct and never met:
//!
//! - In memory, a delta apply builds its marker WITH the sentinel banner
//!   (`AgentContext::compact_with_delta_summary`).
//! - To the DB, the persist path wrote `CompactionOutcome::marker(..)`, whose
//!   banner is hardcoded to the FULL-WINDOW form.
//!
//! So a summary covering only the delta was stored under a banner claiming to
//! cover "everything before this point". The reload anchors on the banner, so
//! every earlier frozen segment was dropped — on the next TURN, not merely on
//! restart, because `run_tool_loop` rebuilds the context from the DB each turn.
//!
//! These tests drive the PRODUCER (`marker_row_to_persist`) and then feed its
//! output to the CONSUMER (`messages_from_last_compaction`). A fixture authored
//! by hand to match what the consumer expects, while nothing ever emits it,
//! cannot fail — which is exactly how this shipped.

use crate::brain::agent::context::{
    AgentContext, COMPACTION_MARKER_PREFIX, CompactionScope, DELTA_MARKER_PREFIX,
};
use crate::brain::agent::service::AgentService;
use crate::brain::agent::service::compaction::{CompactionOutcome, marker_row_to_persist};
use crate::brain::provider::Message;

fn ctx(max_tokens: usize) -> AgentContext {
    AgentContext::new(uuid::Uuid::nil(), max_tokens)
}

/// A DB stream row shaped like the real writers (#175: `user` role, content
/// BEGINNING with the marker prefix).
fn db_row(content: String) -> crate::db::models::Message {
    crate::db::models::Message {
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::nil(),
        role: "user".to_string(),
        content,
        sequence: 0,
        created_at: chrono::Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: None,
        duration_secs: None,
    }
}

/// Apply a full-window compaction and return the row the persist path writes.
fn persist_full_window(c: &mut AgentContext, summary: &str) -> String {
    AgentService::apply_scoped_compaction_summary(c, CompactionScope::FullWindow, summary);
    marker_row_to_persist(c, &CompactionOutcome::Summarised(summary.to_string()), "")
}

/// Apply a delta compaction and return the row the persist path writes.
fn persist_delta(c: &mut AgentContext, summary: &str) -> String {
    AgentService::apply_scoped_compaction_summary(c, CompactionScope::DeltaSinceMarker, summary);
    marker_row_to_persist(c, &CompactionOutcome::Summarised(summary.to_string()), "")
}

#[test]
fn a_delta_summary_persists_under_the_delta_banner() {
    let mut c = ctx(200_000);
    c.add_message(Message::user("q1"));
    c.add_message(Message::assistant("a1"));
    persist_full_window(&mut c, "SUMMARY ONE");
    c.add_message(Message::user("q2"));

    let row = persist_delta(&mut c, "SUMMARY TWO");

    assert!(
        row.starts_with(DELTA_MARKER_PREFIX),
        "the delta summary was persisted under a banner that lies about its \
         scope, so the reload will drop every earlier segment: {row}"
    );
    assert!(row.contains("SUMMARY TWO"), "the summary body must survive");
}

#[test]
fn a_full_window_summary_does_not_carry_the_delta_banner() {
    let mut c = ctx(200_000);
    c.add_message(Message::user("q1"));

    let row = persist_full_window(&mut c, "SUMMARY ONE");

    assert!(row.starts_with(COMPACTION_MARKER_PREFIX));
    assert!(
        !row.starts_with(DELTA_MARKER_PREFIX),
        "a full-window marker must stay a boundary: {row}"
    );
}

#[test]
fn truncation_still_persists_a_boundary_notice() {
    // Truncation drops messages WITHOUT installing an in-memory marker, so the
    // notice is the only record that history was lost. It must stay a boundary.
    let mut c = ctx(200_000);
    c.add_message(Message::user("q1"));

    let row = marker_row_to_persist(&c, &CompactionOutcome::Truncated, "");

    assert!(row.starts_with(COMPACTION_MARKER_PREFIX));
    assert!(!row.starts_with(DELTA_MARKER_PREFIX));
    assert!(row.contains("Nothing before this point survives"));
}

#[test]
fn the_persisted_rows_reload_with_every_earlier_segment_kept() {
    let mut c = ctx(200_000);
    c.add_message(Message::user("q1"));
    let boundary = persist_full_window(&mut c, "SUMMARY ONE");
    c.add_message(Message::user("q2"));
    let segment = persist_delta(&mut c, "SUMMARY TWO");

    let all = vec![
        db_row("ancient history".to_string()),
        db_row(boundary),
        db_row("q2".to_string()),
        db_row(segment),
        db_row("live tail".to_string()),
    ];

    let kept = AgentService::messages_from_last_compaction(all);

    // The old producer wrote the segment under the full-window banner, so the
    // anchor landed on it and this was 2 (the segment + the tail) — SUMMARY ONE
    // and everything it described were gone.
    assert_eq!(
        kept.len(),
        4,
        "the delta segment must EXTEND the boundary, not replace it"
    );
    assert!(kept[0].content.starts_with(COMPACTION_MARKER_PREFIX));
    assert!(kept[1].content.contains("q2"));
    assert!(kept[2].content.starts_with(DELTA_MARKER_PREFIX));
    assert!(kept[3].content.contains("live tail"));
}

#[test]
fn a_boundary_whose_summary_body_quotes_the_sentinel_still_anchors_the_window() {
    // The skip test used to be `content.contains(SEGMENT_SENTINEL)` over the
    // whole row, body included. A full-window marker whose PROSE mentions the
    // sentinel — a lane quoting one out of a log — was then skipped as if it
    // were a segment, pushing the anchor back or reloading everything (#175).
    let prose = "we discussed the DELTA SEGMENT. banner while quoting a log";
    let mut c = ctx(200_000);
    c.add_message(Message::user("q1"));

    let boundary = persist_full_window(&mut c, prose);
    assert!(boundary.contains("DELTA SEGMENT."), "fixture must quote it");
    assert!(!boundary.starts_with(DELTA_MARKER_PREFIX));

    let all = vec![db_row(boundary), db_row("kept history".to_string())];
    let kept = AgentService::messages_from_last_compaction(all);

    assert_eq!(
        kept.len(),
        2,
        "a `contains` scan treated a full-window marker as a segment"
    );
    assert!(kept[0].content.starts_with(COMPACTION_MARKER_PREFIX));
}

#[test]
fn a_caller_note_is_appended_after_the_banner_never_before_it() {
    let mut c = ctx(200_000);
    c.add_message(Message::user("q1"));
    AgentService::apply_scoped_compaction_summary(
        &mut c,
        CompactionScope::FullWindow,
        "SUMMARY ONE",
    );

    let row = marker_row_to_persist(
        &c,
        &CompactionOutcome::Summarised("SUMMARY ONE".to_string()),
        " after token calibration revealed high context usage",
    );

    assert!(row.starts_with(COMPACTION_MARKER_PREFIX));
    assert!(!row.starts_with(DELTA_MARKER_PREFIX));
    assert!(row.contains("after token calibration revealed high context usage"));
}
