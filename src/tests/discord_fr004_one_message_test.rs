//! FR-004 / AC-008 / AC-009: one progress card per turn, mechanical counts.
//!
//! Both claims are structural rather than behavioural, so they are pinned
//! structurally:
//!
//! - AC-008: a turn with ten tools adds ZERO messages beyond the single
//!   progress card. The card is created once and edited in place afterwards.
//!   The decision lives in the `ProgressEvent::ToolStarted` arm of
//!   `handler.rs`, where a stored message id selects edit over create, so the
//!   scan below reads that arm and fails the build if a second `writes::say`
//!   appears there.
//! - AC-009 / NFR-003: the tool count shown in the card never comes from model
//!   output. `summary_line` and `render_content` are pure functions of
//!   `GroupState`, whose entries come from the tool-execution stream, so the
//!   rendered count equals `entries.len()` at every size.

use std::path::Path;
use std::time::Instant;

use crate::channels::discord::tool_group::{GroupEntry, GroupState, render_content};

fn group(n: usize, expanded: bool) -> GroupState {
    GroupState {
        last_activity_at: Instant::now(),
        entries: (0..n)
            .map(|i| GroupEntry {
                name: format!("tool{i}"),
                context: format!(" (arg{i})"),
                status: Some(true),
            })
            .collect(),
        notes: Vec::new(),
        expanded,
        started_at: Instant::now(),
        live_ctx: None,
        settled: None,
    }
}

/// Flattened source: all whitespace removed, so a signature or a call split
/// across lines still matches a single-line needle.
fn flattened(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// AC-009: the number in the card is `entries.len()` and nothing else. A model
/// answer is never an input, so no size can drift from its entry count.
#[test]
fn the_rendered_count_is_exactly_the_number_of_tool_entries() {
    // A single un-expanded tool renders as a bare line with no count at all,
    // so the count only has to be exact once the card actually shows one.
    for n in [2usize, 3, 10, 25] {
        let rendered = render_content(&group(n, false));
        let expected = format!("{n} tool calls");
        assert!(
            rendered.contains(&expected),
            "AC-009: a group of {n} entries must render {expected:?}, got {rendered:?}"
        );
    }
    // One entry with the list open still reports the singular "1 tool call".
    let rendered = render_content(&group(1, true));
    assert!(
        rendered.contains("1 tool call"),
        "AC-009: a single-entry group with the list open must render the singular \
         \"1 tool call\", got {rendered:?}"
    );
}

/// AC-009 / NFR-003, the structural half: the count can only come from the
/// entries while the render path takes no other input. A future change that
/// parsed model text for a count would have to widen these signatures first,
/// and this test turns that into a build failure instead of a silent drift.
#[test]
fn the_render_path_takes_no_model_text() {
    let flat = flattened("src/channels/discord/tool_group.rs");
    for sig in [
        "fnsummary_line(group:&GroupState)->String",
        "fnrender_content(group:&GroupState)->String",
    ] {
        assert!(
            flat.contains(sig),
            "AC-009: {sig} is gone. If the render path gained an input, prove the \
             count still cannot come from model output before removing this guard"
        );
    }
}

/// AC-008: one card per turn. The `ProgressEvent::ToolStarted` arm creates the
/// card exactly once and edits the stored message for every later tool, so a
/// second `writes::say` in that arm means a second message per turn.
#[test]
fn the_tool_started_arm_creates_one_card_and_edits_after() {
    let flat = flattened("src/channels/discord/handler.rs");

    let start = flat
        .find("ProgressEvent::ToolStarted")
        .expect("handler.rs must still handle ProgressEvent::ToolStarted");
    let rest = &flat[start..];
    let end = rest
        .find("ProgressEvent::ToolCompleted")
        .expect("ToolCompleted must follow ToolStarted in the same match");
    let arm = &rest[..end];

    let creates = arm.matches("writes::say(").count();
    let edits = arm.matches("writes::edit(").count();

    assert_eq!(
        creates, 1,
        "AC-008: the ToolStarted arm must create exactly ONE progress card per \
         turn, found {creates} `writes::say` calls. Later progress has to edit \
         the stored message instead of posting another."
    );
    assert!(
        edits >= 1,
        "AC-008: the ToolStarted arm must edit the stored card once a message id \
         is known, found {edits} `writes::edit` calls"
    );
}
