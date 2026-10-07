//! The poll loop's `allowed_updates` allowlist, its dispatcher branches and its
//! debug-log names have to agree (#109).
//!
//! Telegram delivers nothing outside `allowed_updates`, and the dispatcher
//! drops any update it has no branch for, so a kind is only really subscribed
//! when three places agree. The drift was already real before this guard:
//! `my_chat_member` was subscribed and branched on, but `update_kind_name`
//! returned "other" for it, so the poll log could not name an update the loop
//! was actively receiving.

use crate::channels::telegram::raw_updates::ALLOWED_UPDATES;

/// The `Update::filter_*` branch each subscribed kind is expected to have.
/// `message_reaction` is the one kind whose Bot API name and teloxide filter
/// name differ, which is exactly the mismatch a naive string match would miss.
const EXPECTED_FILTERS: &[(&str, &str)] = &[
    ("message", "filter_message"),
    ("edited_message", "filter_edited_message"),
    ("callback_query", "filter_callback_query"),
    ("message_reaction", "filter_message_reaction_updated"),
    ("my_chat_member", "filter_my_chat_member"),
];

const AGENT_SRC: &str = include_str!("../channels/telegram/agent.rs");
const RAW_SRC: &str = include_str!("../channels/telegram/raw_updates.rs");

/// The body of `update_kind_name`, so a literal elsewhere in the file (the
/// allowlist itself, for one) cannot satisfy the name check by accident.
fn update_kind_name_body() -> &'static str {
    let start = RAW_SRC
        .find("fn update_kind_name")
        .expect("update_kind_name must exist in raw_updates.rs");
    let rest = &RAW_SRC[start..];
    let end = rest
        .find("\n}\n")
        .expect("update_kind_name must end with a closing brace on its own line");
    &rest[..end]
}

#[test]
fn allowed_updates_has_no_duplicates() {
    let mut seen = std::collections::HashSet::new();
    for kind in ALLOWED_UPDATES {
        assert!(seen.insert(*kind), "{kind} is listed twice in ALLOWED_UPDATES");
    }
}

#[test]
fn every_subscribed_kind_has_a_dispatcher_branch() {
    for (kind, filter) in EXPECTED_FILTERS {
        assert!(
            ALLOWED_UPDATES.contains(kind),
            "{kind} has an expected dispatcher branch but is not subscribed"
        );
        assert!(
            AGENT_SRC.contains(&format!("Update::{filter}()")),
            "{kind} is subscribed but agent.rs has no Update::{filter}() branch, so the \
             dispatcher drops every one of its updates"
        );
    }
}

#[test]
fn every_subscribed_kind_is_named_in_the_poll_log() {
    let body = update_kind_name_body();
    for kind in ALLOWED_UPDATES {
        assert!(
            body.contains(&format!("\"{kind}\"")),
            "{kind} is subscribed but update_kind_name cannot name it, so it logs as \"other\""
        );
    }
}

/// Guards the table above rather than the code: a kind subscribed without being
/// added to `EXPECTED_FILTERS` would otherwise let both lists drift apart
/// silently, and the branch check would simply stop covering it.
#[test]
fn expected_filters_covers_every_subscribed_kind() {
    for kind in ALLOWED_UPDATES {
        assert!(
            EXPECTED_FILTERS.iter().any(|(k, _)| k == kind),
            "{kind} is subscribed but missing from EXPECTED_FILTERS"
        );
    }
}

#[test]
fn update_kind_name_body_is_extracted_correctly() {
    // Negative control for the extractor: if it ever returns an empty or wrong
    // region, `every_subscribed_kind_is_named_in_the_poll_log` would pass
    // vacuously instead of failing.
    let body = update_kind_name_body();
    assert!(
        body.contains("\"message\""),
        "extracted region does not look like update_kind_name: {body}"
    );
    assert!(
        body.contains("ERROR(unparsed)"),
        "extracted region does not look like update_kind_name: {body}"
    );
}
