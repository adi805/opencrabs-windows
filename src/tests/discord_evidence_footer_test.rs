//! FR-007 (#1880): the mechanical evidence footer on a Discord answer.
//!
//! The footer is built from the turn's tool-group entries (the tools that
//! ACTUALLY ran, appended from `ProgressEvent::ToolStarted`), never from the
//! model's prose (NFR-003). It is assembled at the channel layer after the
//! agent returned, so the phantom gate never inspects it; the wording is
//! additionally pinned against every `executed_framings` entry so the line
//! can never read as an "already ran" claim (AC-016).
//!
//! The line reports category COUNTS (`🛠️ baca 2 file · jalan 3 perintah`)
//! rather than tool names: identifiers are machine vocabulary, and the
//! reader wants to know how much work happened, not which function ran.

use crate::brain::agent::service::phantom_lang::all_langs;
use crate::channels::discord::tool_group::{GroupEntry, GroupState, evidence_line};
use crate::channels::evidence::EVIDENCE_HEADER;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The shared renderer's own literals. A second evidence path is a copy of
/// these, and a copy is free to drift from the phantom-safety property that
/// `footer_never_frames_a_command_as_already_run` pins.
const RENDERER_LITERALS: &[&str] = &[
    EVIDENCE_HEADER,
    "baca {count} file",
    "jalan {count} perintah",
    "tulis {count} file",
    "lainnya {count}",
];

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

/// The footer for a body, composed exactly as the renderer composes it.
fn expect_line(body: &str) -> String {
    format!("{EVIDENCE_HEADER} {body}")
}

/// AC-014: an answer that used tools reports the work it did, as category
/// counts, not as a list of tool identifiers.
#[test]
fn reports_each_category_with_its_count() {
    let names = ["read_file", "bash", "write_file", "web_search"];
    let line = evidence_line(&group(&names)).expect("four tools must yield a line");
    let body = "baca 1 file · jalan 1 perintah · tulis 1 file · lainnya 1";
    assert_eq!(line, expect_line(body), "one segment per populated bucket");
}

/// AC-015: a turn that ran no tools shows NO footer. An empty or invented
/// line is worse than none: it would read as evidence that does not exist.
#[test]
fn no_tools_means_no_footer() {
    assert!(
        evidence_line(&group(&[])).is_none(),
        "a tool-less turn must not render an evidence line"
    );
}

/// Repetition now COUNTS. The reader asking "how much work happened" wants
/// `jalan 4 perintah`, not a deduped single mention.
#[test]
fn repeated_tools_are_counted() {
    let names = ["bash", "bash", "bash", "bash"];
    let line = evidence_line(&group(&names)).expect("four calls must yield a line");
    assert_eq!(line, expect_line("jalan 4 perintah"));
}

/// A bucket with no actions is omitted, so a read-only turn is one short
/// phrase rather than a row of zeros.
#[test]
fn empty_buckets_are_omitted() {
    let names = ["read_file", "grep"];
    let line = evidence_line(&group(&names)).expect("two reads yield a line");
    assert_eq!(line, expect_line("baca 2 file"));
    for absent in ["jalan", "tulis", "lainnya"] {
        assert!(
            !line.contains(absent),
            "empty bucket {absent} leaked: {line}"
        );
    }
}

/// A tool nobody has categorised yet lands in `lainnya` instead of being
/// dropped: the footer accounts for EVERY action the turn took, so a newly
/// added tool cannot silently vanish from the receipt.
#[test]
fn unknown_tools_land_in_other_rather_than_vanishing() {
    let names = ["some_future_tool"];
    let line = evidence_line(&group(&names)).expect("unknown tool must yield a line");
    assert_eq!(line, expect_line("lainnya 1"));
}

/// The render order is fixed by the bucket list, not by the order the tools
/// happened to run in, so the footer reads the same way every turn.
#[test]
fn order_is_fixed_regardless_of_call_order() {
    let run_first = ["bash", "read_file"];
    let read_first = ["read_file", "bash"];
    let forward = evidence_line(&group(&run_first)).expect("two tools yield a line");
    let backward = evidence_line(&group(&read_first)).expect("two tools yield a line");
    assert_eq!(forward, backward, "call order must not change the footer");
    let read_at = forward.find("baca");
    let run_at = forward.find("jalan");
    assert!(read_at < run_at, "read bucket must render first: {forward}");
}

/// The old `+N more` cap is gone because the buckets are bounded: a long
/// turn still renders four short phrases, never a wall of identifiers.
#[test]
fn long_turns_stay_bounded() {
    let mut names: Vec<&str> = vec!["read_file", "ls", "glob", "grep"];
    names.extend(["bash", "bash"]);
    names.extend(["write_file", "edit_file"]);
    names.extend(["a1", "a2", "a3", "a4", "a5", "a6"]);
    names.extend(["b1", "b2", "b3", "b4", "b5", "b6"]);
    let line = evidence_line(&group(&names)).expect("twenty tools must yield a line");
    let body = "baca 4 file · jalan 2 perintah · tulis 2 file · lainnya 12";
    assert_eq!(line, expect_line(body), "every bucket must be counted");
    assert!(
        !line.contains("more"),
        "the removed cap must not reappear: {line}"
    );
}

/// AC-016: the footer must not read as a claim that a command ALREADY RAN.
/// The phantom gate keys on `executed_framings` ("checked with", "verified
/// with", "ran ", …); the WHOLE rendered line (header and every bucket
/// label) is pinned against every language's list, so a future rewording
/// cannot quietly re-introduce one.
#[test]
fn footer_never_frames_a_command_as_already_run() {
    let names = ["read_file", "bash", "write_file", "web_search"];
    let line = evidence_line(&group(&names)).expect("four tools yield a line");
    let rendered = line.to_lowercase();
    for lang in all_langs() {
        for framing in &lang.executed_framings {
            assert!(
                !rendered.contains(framing.as_str()),
                "footer {rendered:?} contains the executed-framing {framing:?}"
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
            !src.contains(EVIDENCE_HEADER),
            "{name} hardcodes the footer header instead of using EVIDENCE_HEADER"
        );
    }
}

/// NFR-004 / AC-020: `src/channels/evidence.rs` must stay the ONLY renderer.
///
/// The two-channel pin above names its files by hand, so it only covers the
/// surfaces that existed when it was written: Fase 2's autocomplete and
/// context menus are exactly the kind of new interactive surface that could
/// render its own footer and pass that test unnoticed. This sweeps every
/// `.rs` under `src/channels/` instead of listing channels, so a surface
/// added later is covered the moment it exists.
///
/// Two failure shapes, both a "second evidence path":
///
/// 1. The renderer's own literals (the header, the four bucket templates)
///    appearing outside `evidence.rs`. A duplicated renderer is a copy of
///    these, and a copy is free to drift from the phantom-safety property
///    `footer_never_frames_a_command_as_already_run` pins.
/// 2. A local `fn evidence_line` that does not delegate to the shared one.
///    Discord's `tool_group.rs` legitimately carries such a name, so the
///    check is delegation, not existence.
#[test]
fn evidence_has_a_single_renderer_across_every_channel() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels");
    let mut files = Vec::new();
    collect_rs_files(&root, &mut files);
    assert!(
        files.len() > 20,
        "source walk found {} files under {:?}; cwd={:?}",
        files.len(),
        root,
        std::env::current_dir()
    );

    let mut violations = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        if rel == "evidence.rs" {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(file) else {
            continue;
        };
        for needle in RENDERER_LITERALS {
            if src.contains(needle) {
                violations.push(format!("{rel} re-renders {needle:?}"));
            }
        }
        if src.contains("fn evidence_line") && !src.contains("evidence::evidence_line(") {
            violations.push(format!(
                "{rel} defines evidence_line without delegating to evidence.rs"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "NFR-004/AC-020: src/channels/evidence.rs must be the only renderer, found: {violations:?}"
    );
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
