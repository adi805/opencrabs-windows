//! A ratchet on how many channel actions have no test naming them.
//!
//! Measured on the branch that introduces this test. `origin/main` measured
//! 43/24/12/11 just before it; this PR's own sweeps name `wa_introspect`, which
//! is why whatsapp sits one lower here than on main:
//!
//! | tool          | actions | untested |
//! |---------------|---------|----------|
//! | whatsapp_send | 139     | 42       |
//! | telegram_send | 48      | 24       |
//! | discord_send  | 28      | 12       |
//! | slack_send    | 17      | 11       |
//!
//! That is 89 of 232 actions. The counts are pinned rather than fixed, because
//! a ratchet that fails on a pre-existing gap is a test nobody can keep green;
//! a ratchet that fails when the gap GROWS is one that cannot be ignored.
//!
//! A new action therefore arrives with a test naming it, or with a visible bump
//! to the number below, which a reviewer sees in the diff. Lowering a number
//! when coverage improves is welcome and costs nothing.

use std::collections::BTreeSet;

/// `(tool, source, budget)`.
const TOOLS: &[(&str, &str, usize)] = &[
    (
        "whatsapp_send",
        include_str!("../brain/tools/whatsapp_send.rs"),
        42,
    ),
    (
        "telegram_send",
        include_str!("../brain/tools/telegram_send.rs"),
        24,
    ),
    (
        "discord_send",
        include_str!("../brain/tools/discord_send.rs"),
        12,
    ),
    (
        "slack_send",
        include_str!("../brain/tools/slack_send.rs"),
        11,
    ),
];

/// The action list: the first `"enum": [ ... ]` array in a channel tool.
fn declared_actions(src: &str) -> Vec<String> {
    let Some(rel) = src.find("\"enum\": [") else {
        return Vec::new();
    };
    let at = rel + "\"enum\": [".len();
    let Some(end) = src[at..].find(']') else {
        return Vec::new();
    };
    let body = &src[at..at + end];
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = body[from..].find('"') {
        let open = from + rel + 1;
        let Some(close) = body[open..].find('"') else {
            break;
        };
        out.push(body[open..open + close].to_string());
        from = open + close + 1;
    }
    out
}

/// Every test source, concatenated. A name that appears anywhere in here counts
/// as covered, which is deliberately generous: the point is to catch an action
/// nobody wrote a single assertion about.
fn all_test_sources() -> String {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tests");
    let mut out = String::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            out.push_str(&text);
            out.push('\n');
        }
    }
    out
}

/// A guard on the reader: if the test tree stops being found, every count below
/// would read as "fully covered" and the ratchet would silently stop working.
#[test]
fn the_test_tree_is_readable() {
    let all = all_test_sources();
    assert!(
        all.len() > 1_000_000,
        "read only {} bytes of test sources, so the reader is not finding them",
        all.len()
    );
}

#[test]
fn no_tool_regresses_on_untested_actions() {
    let all = all_test_sources();
    let mut regressions = Vec::new();
    for (name, src, budget) in TOOLS {
        let actions = declared_actions(src);
        let unique: BTreeSet<&String> = actions.iter().collect();
        assert_eq!(
            actions.len(),
            unique.len(),
            "{name}: the action list repeats a name"
        );
        let untested = actions
            .iter()
            .filter(|a| !all.contains(&format!("\"{a}\"")))
            .count();
        if untested > *budget {
            regressions.push(format!(
                "{name}: {untested} actions have no test naming them, up from the pinned {budget}"
            ));
        }
    }
    assert!(
        regressions.is_empty(),
        "the number of untested channel actions grew ({}). Add a test that names the new action, \
         or raise the pinned budget in this file so the increase is visible in the diff.",
        regressions.join("; ")
    );
}
