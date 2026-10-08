//! The poll loop's `allowed_updates` allowlist, its dispatcher branches and its
//! debug-log names have to agree (#109).
//!
//! Telegram delivers nothing outside `allowed_updates`, and the dispatcher
//! drops any update it has no branch for, so a kind is only really subscribed
//! when three places agree. The drift was already real before this guard:
//! `my_chat_member` was subscribed and branched on, but `update_kind_name`
//! returned "other" for it, so the poll log could not name an update the loop
//! was actively receiving.
//!
//! One kind is exempt from the typed half of that agreement. Bot API 10.3
//! `stopped_message_generation` has no `UpdateKind` variant in teloxide-core
//! 0.13, so no `Update::filter_*` can exist for it and `update_kind_name`
//! (which matches on `UpdateKind`) can never name it; the poll loop reads it
//! off the raw envelope instead. `OFF_ENVELOPE_KINDS` holds those exemptions,
//! and `off_envelope_kinds_are_read_off_the_envelope` pins each one to a real
//! reader, so the list cannot quietly excuse a branch that was forgotten.

use crate::channels::telegram::raw_updates::{ALLOWED_UPDATES, raw_update_kind_name};

/// The `Update::filter_*` branch each subscribed kind is expected to have.
/// `message_reaction` is the one kind whose Bot API name and teloxide filter
/// name differ, which is exactly the mismatch a naive string match would miss.
const EXPECTED_FILTERS: &[(&str, &str)] = &[
    ("message", "filter_message"),
    ("edited_message", "filter_edited_message"),
    ("callback_query", "filter_callback_query"),
    ("message_reaction", "filter_message_reaction_updated"),
    ("my_chat_member", "filter_my_chat_member"),
    ("chat_join_request", "filter_chat_join_request"),
    ("inline_query", "filter_inline_query"),
];

/// Subscribed kinds the typed dispatcher cannot handle at all, with the call
/// the raw poll loop makes to read each one off the envelope.
///
/// An exemption is not a dumping ground: a kind belongs here only when
/// teloxide-core has no variant for it, which is why no `Update::filter_*` can
/// be written. A kind whose branch was merely forgotten is not on this list,
/// so both guards below still fail for it.
const OFF_ENVELOPE_KINDS: &[(&str, &str)] = &[
    // (Bot API name, the raw reader the poll loop calls)
    ("stopped_message_generation", "stopped_message_generation("),
];

const AGENT_SRC: &str = include_str!("../channels/telegram/agent.rs");
const RAW_SRC: &str = include_str!("../channels/telegram/raw_updates.rs");

fn is_off_envelope(kind: &str) -> bool {
    OFF_ENVELOPE_KINDS.iter().any(|(k, _)| *k == kind)
}

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
        assert!(
            seen.insert(*kind),
            "{kind} is listed twice in ALLOWED_UPDATES"
        );
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
        if is_off_envelope(kind) {
            // `update_kind_name` matches on `UpdateKind`, which has no variant
            // for these. `raw_update_kind_name` names them instead, and the
            // pin below proves that it actually does.
            continue;
        }
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
            is_off_envelope(kind) || EXPECTED_FILTERS.iter().any(|(k, _)| k == kind),
            "{kind} is subscribed but missing from EXPECTED_FILTERS, and it is not listed \
             as off-envelope either, so nothing would notice a missing dispatcher branch"
        );
    }
}

/// An exemption is only honest while the kind really is read off the envelope.
/// Pin each one to a reader the poll loop calls and to a name the raw poll log
/// can produce, so the list cannot excuse a kind that nothing handles.
#[test]
fn off_envelope_kinds_are_read_off_the_envelope() {
    for (kind, reader) in OFF_ENVELOPE_KINDS {
        assert!(
            ALLOWED_UPDATES.contains(kind),
            "{kind} is excused from the typed branch but is not subscribed"
        );
        assert!(
            !EXPECTED_FILTERS.iter().any(|(k, _)| k == kind),
            "{kind} is both off-envelope and in EXPECTED_FILTERS: pick one"
        );
        assert!(
            RAW_SRC.contains(*reader),
            "{kind} is excused but raw_updates.rs never calls {reader}"
        );

        // Behaviour, not source text: the raw poll log must really name it.
        let payload = serde_json::json!({"chat": {"id": 1}, "draft_id": 1});
        let mut probe = serde_json::Map::new();
        let replaced = probe.insert(String::from(*kind), payload);
        assert!(replaced.is_none(), "the probe must carry one key only");
        assert_eq!(
            raw_update_kind_name(&serde_json::Value::Object(probe)),
            *kind,
            "{kind} is subscribed but the raw poll log cannot name it"
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
