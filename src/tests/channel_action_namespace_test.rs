//! Namespace collisions across the channel tool surfaces.
//!
//! The same mistake has shipped three times: a name introduced into one
//! namespace that already existed in a sibling one. A duplicate `fn
//! poll_options`, a `send_app_state_action` sub-op named `clear_chat` shadowing
//! the top-level `clear_chat` action, and `is_connected` defined in two `impl`
//! blocks. The first two are caught here; the third by
//! `channel_impl_method_test`.
//!
//! The rule this file enforces: the FIRST `"enum": [ ... ]` array in a channel
//! tool is the action list the model can discover. Every other array in the
//! same file declares a sub-op namespace (disappearing timers, app-state ops,
//! introspection ops). A value in one of those must never equal a top-level
//! action name.
//!
//! It matters because the source-shape tests look an action up by its quoted
//! name and take the FIRST occurrence. When a sub-op shares that name, the
//! helper asserts against the sub-op's line instead of the action's arm, and
//! the check silently stops covering what it claims to cover.

use std::collections::BTreeSet;

const TOOLS: &[(&str, &str)] = &[
    (
        "whatsapp_send",
        include_str!("../brain/tools/whatsapp_send.rs"),
    ),
    (
        "telegram_send",
        include_str!("../brain/tools/telegram_send.rs"),
    ),
    (
        "discord_send",
        include_str!("../brain/tools/discord_send.rs"),
    ),
    ("slack_send", include_str!("../brain/tools/slack_send.rs")),
];

/// Every `"enum": [ ... ]` array body in a source, in file order.
fn enum_bodies(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = src[from..].find("\"enum\": [") {
        let at = from + rel + "\"enum\": [".len();
        match src[at..].find(']') {
            Some(end) => {
                out.push(src[at..at + end].to_string());
                from = at + end + 1;
            }
            None => break,
        }
    }
    out
}

/// The double-quoted literals in a fragment, in order.
fn quoted(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = body[from..].find('"') {
        let at = from + rel + 1;
        match body[at..].find('"') {
            Some(end) => {
                out.push(body[at..at + end].to_string());
                from = at + end + 1;
            }
            None => break,
        }
    }
    out
}

/// The action list: the first enum array in the file.
fn declared_actions(src: &str) -> Vec<String> {
    match enum_bodies(src).first() {
        Some(body) => quoted(body),
        None => Vec::new(),
    }
}

/// A guard on the parser itself: if the first array stops being the action
/// list, every other assertion in this file would pass vacuously.
#[test]
fn the_first_enum_array_is_the_action_list() {
    for (name, src) in TOOLS {
        let actions = declared_actions(src);
        assert!(
            actions.len() > 10,
            "{name}: the first enum array holds {} entries, too few to be the action list",
            actions.len()
        );
    }
}

#[test]
fn no_sub_op_shadows_a_top_level_action() {
    for (name, src) in TOOLS {
        let enums = enum_bodies(src);
        let top: BTreeSet<String> = declared_actions(src).into_iter().collect();
        for (i, body) in enums.iter().enumerate().skip(1) {
            for value in quoted(body) {
                assert!(
                    !top.contains(&value),
                    "{name}: sub-op '{value}' (enum array #{i}) has the same name as a top-level \
                     action. Rename the sub-op, because a lookup by that name lands on the wrong \
                     arm and the check that claims to cover the action stops covering it."
                );
            }
        }
    }
}

#[test]
fn action_names_are_lowercase_ascii_without_digits() {
    for (name, src) in TOOLS {
        for action in declared_actions(src) {
            assert!(
                action.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name}: action '{action}' must be lowercase ASCII with underscores only. A digit \
                 or a capital makes the README action-count test drop the token, so the declared \
                 count silently disagrees with the enum."
            );
        }
    }
}

#[test]
fn the_action_list_has_no_duplicates() {
    for (name, src) in TOOLS {
        let actions = declared_actions(src);
        let unique: BTreeSet<&String> = actions.iter().collect();
        assert_eq!(
            actions.len(),
            unique.len(),
            "{name}: the action list repeats a name ({} entries, {} unique)",
            actions.len(),
            unique.len()
        );
    }
}
