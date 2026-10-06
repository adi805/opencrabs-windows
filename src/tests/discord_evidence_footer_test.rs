//! FR-007 (#1880): the mechanical evidence footer on a Discord answer.
//!
//! The footer is built from the turn's tool-group entries — the tools that
//! ACTUALLY ran, appended from `ProgressEvent::ToolStarted` — never from the
//! model's prose (NFR-003). It is assembled at the channel layer after the
//! agent returned, so the phantom gate never inspects it; the wording is
//! additionally pinned against every `executed_framings` entry so the line
//! can never read as an "already ran" claim (AC-016).

use crate::brain::agent::service::phantom_lang::all_langs;
use crate::channels::discord::tool_group::{GroupEntry, GroupState, evidence_line};
use crate::channels::evidence::EVIDENCE_HEADER;
use std::time::Instant;

fn group(names: &[&str]) -> GroupState {
    GroupState {
        entries: names
            .iter()
            .map(|n| GroupEntry {
                name: (*n).to_string(),
                context: String::new(),
                status: Some(true),
            })
            .collect(),
        notes: Vec::new(),
        expanded: false,
        started_at: Instant::now(),
        settled: None,
        last_activity_at: Instant::now(),
    }
}

/// AC-014: an answer that used tools names the tools it used.
#[test]
fn names_every_tool_the_turn_ran() {
    let line = evidence_line(&group(&["read_file", "bash"])).expect("two tools must yield a line");
    assert!(line.contains("read_file"), "missing first tool: {line}");
    assert!(line.contains("bash"), "missing second tool: {line}");
    assert!(
        line.starts_with(EVIDENCE_HEADER),
        "footer must lead with the pinned header: {line}"
    );
}

/// AC-015: a turn that ran no tools shows NO footer. An empty or invented
/// line is worse than none — it would read as evidence that does not exist.
#[test]
fn no_tools_means_no_footer() {
    assert!(
        evidence_line(&group(&[])).is_none(),
        "a tool-less turn must not render an evidence line"
    );
}

/// A turn that reads the same file four times is still ONE tool in the
/// evidence line; repetition is not evidence of breadth.
#[test]
fn repeated_tools_are_named_once() {
    let line = evidence_line(&group(&["bash", "bash", "bash"])).expect("one tool yields a line");
    assert_eq!(line.matches("bash").count(), 1, "duplicate name in: {line}");
}

/// A 20-tool turn must not turn its footer into a wall.
#[test]
fn long_turns_are_capped() {
    let names: Vec<String> = (0..9).map(|i| format!("tool{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let line = evidence_line(&group(&refs)).expect("nine tools yield a line");
    assert!(line.contains("+3 more"), "cap not applied: {line}");
    assert!(line.contains("tool5"), "first six must be shown: {line}");
    assert!(!line.contains("tool6"), "seventh must be folded: {line}");
}

/// AC-016: the footer must not read as a claim that a command ALREADY RAN.
/// The phantom gate keys on `executed_framings` ("checked with", "verified
/// with", "ran ", …); the header is pinned against every language's list so
/// a future rewording cannot quietly re-introduce one.
#[test]
fn footer_never_frames_a_command_as_already_run() {
    let lower = EVIDENCE_HEADER.to_lowercase();
    for lang in all_langs() {
        for framing in &lang.executed_framings {
            assert!(
                !lower.contains(framing.as_str()),
                "evidence header {EVIDENCE_HEADER:?} contains the executed-framing {framing:?}"
            );
        }
    }
}

/// NFR-002: the footer must land on BOTH surfaces from ONE implementation. A
/// channel that re-implemented the line would be free to drift from the
/// phantom-safety property the test above pins.
#[test]
fn both_channels_render_the_shared_line() {
    const DISCORD: &str = include_str!("../channels/discord/tool_group.rs");
    const TELEGRAM: &str = include_str!("../channels/telegram/delivery.rs");
    for (name, src) in [("discord", DISCORD), ("telegram", TELEGRAM)] {
        assert!(
            src.contains("evidence::evidence_line("),
            "{name} does not render the shared evidence line"
        );
        assert!(
            !src.contains("🔎 evidence:"),
            "{name} hardcodes the footer header instead of using EVIDENCE_HEADER"
        );
    }
}
