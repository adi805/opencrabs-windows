//! Group lifecycle actions on the WhatsApp send tool.
//!
//! The arms themselves need a live socket, so the wire calls cannot be
//! exercised here. What IS checkable without a connection is the part that
//! decides what the model can reach and where an operation lands: the dispatch
//! table, the JID resolution rules, and the refusal to guess a target for an
//! admin action.

use crate::brain::tools::whatsapp_send::{
    resolve_group_jid, resolve_participants, summarize_participant_changes,
};
use serde_json::json;
use wacore_binary::jid::Server;

const SOURCE: &str = include_str!("../brain/tools/whatsapp_send.rs");

/// Every action this task added, paired with the `Groups` method it must call.
const ACTIONS: &[(&str, &str)] = &[
    ("create_group", "create_group"),
    ("leave_group", "leave"),
    ("set_group_subject", "set_subject"),
    ("set_group_description", "set_description"),
    ("add_participants", "add_participants"),
    ("remove_participants", "remove_participants"),
    ("promote_participants", "promote_participants"),
    ("demote_participants", "demote_participants"),
    ("get_group_metadata", "get_metadata"),
    ("get_invite_link", "get_invite_link"),
    ("join_with_invite_code", "join_with_invite_code"),
];

/// The source with all whitespace removed, so an assertion does not depend on
/// where rustfmt chose to wrap a chain across lines.
fn compact() -> String {
    SOURCE.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn every_group_action_has_a_match_arm_and_a_schema_entry() {
    let compact = compact();
    for (action, _) in ACTIONS {
        assert!(
            compact.contains(&format!("\"{action}\"=>")),
            "action '{action}' has no match arm: it would fall into the unknown-action error"
        );
        let occurrences = compact.matches(&format!("\"{action}\"")).count();
        assert!(
            occurrences >= 2,
            "'{action}' appears {occurrences} time(s): it needs both a schema enum entry \
             (so the model can discover it) and a match arm"
        );
    }
}

#[test]
fn the_group_arms_call_the_groups_feature() {
    let compact = compact();
    for (action, method) in ACTIONS {
        assert!(
            compact.contains(&format!("groups().{method}(")),
            "'{action}' does not call `client.groups().{method}`"
        );
    }
}

#[test]
fn resolving_a_group_never_falls_back_to_the_owner_or_the_originating_chat() {
    let start = SOURCE
        .find("fn resolve_group_jid")
        .expect("resolve_group_jid is defined");
    let end = SOURCE
        .find("fn resolve_participants")
        .expect("resolve_participants follows it");
    assert!(start < end, "the two helpers are expected in this order");
    let body = &SOURCE[start..end];
    for forbidden in ["owner_jid", "session_jid"] {
        assert!(
            !body.contains(forbidden),
            "resolve_group_jid reaches for `{forbidden}`: a group admin action must name its \
             group, never inherit a target"
        );
    }
}

#[test]
fn a_bare_group_id_becomes_a_group_jid_and_a_missing_one_is_refused() {
    let bare = resolve_group_jid(&json!({"group_jid": "120363012345678901"}))
        .expect("a bare group id is accepted");
    assert_eq!(bare.user.as_str(), "120363012345678901");
    assert_eq!(bare.server, Server::Group);

    let full = resolve_group_jid(&json!({"group_jid": "120363012345678901@g.us"}))
        .expect("a full group JID is accepted");
    assert_eq!(full, bare, "both spellings name the same group");

    assert!(
        resolve_group_jid(&json!({})).is_err(),
        "a group action that names no group must be refused"
    );
}

#[test]
fn participants_accept_numbers_and_jids_and_refuse_junk() {
    let parsed = resolve_participants(&json!({
        "participants": [
            "+15551234567",
            "15559876543@s.whatsapp.net",
            "15550001111:12"
        ]
    }))
    .expect("numbers and JIDs parse");

    let users: Vec<&str> = parsed.iter().map(|j| j.user.as_str()).collect();
    assert_eq!(
        users,
        vec!["15551234567", "15559876543", "15550001111"],
        "a leading '+' and a ':device' suffix are not part of the address"
    );
    for jid in &parsed {
        assert!(
            jid.to_string().contains("@s.whatsapp.net"),
            "{jid} is not a phone JID"
        );
    }

    assert!(
        resolve_participants(&json!({})).is_err(),
        "an absent participant list is refused"
    );
    assert!(
        resolve_participants(&json!({"participants": []})).is_err(),
        "an empty list is refused rather than reported as a no-op"
    );
    assert!(
        resolve_participants(&json!({"participants": ["   "]})).is_err(),
        "a blank entry is refused"
    );
}

#[test]
fn an_empty_change_set_reports_zero_of_zero() {
    assert_eq!(summarize_participant_changes(&[]), "0/0 succeeded");
}
