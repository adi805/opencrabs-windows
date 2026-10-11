//! Group lifecycle actions on the WhatsApp send tool.
//!
//! The arms themselves need a live socket, so the wire calls cannot be
//! exercised here. What IS checkable without a connection is the part that
//! decides what the model can reach and where an operation lands: the dispatch
//! table, the JID resolution rules, and the refusal to guess a target for an
//! admin action.

use crate::brain::tools::whatsapp_send::{
    get_bool, resolve_group_jid, resolve_participants, summarize_participant_changes,
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
    ("set_announce", "set_announce"),
    ("set_locked", "set_locked"),
    ("set_ephemeral", "set_ephemeral"),
    ("set_membership_approval", "set_membership_approval"),
    ("set_member_add_mode", "set_member_add_mode"),
    ("set_limit_sharing", "set_limit_sharing"),
    ("set_group_history", "set_group_history"),
    ("set_no_frequently_forwarded", "set_no_frequently_forwarded"),
    ("set_allow_admin_reports", "set_allow_admin_reports"),
];

/// The policy and admin actions, each with the `Groups` method it must call and
/// the parameter that carries the value it is setting.
///
/// These arms differ from the lifecycle ones in one way that matters: they take
/// the value from the caller instead of picking one, because a group policy that
/// flips on an omitted parameter is a change nobody asked for and there is no
/// undo on the wire. That is what the third element pins.
const POLICY_ACTIONS: &[(&str, &str, &str)] = &[
    ("set_announce", "set_announce", "announce"),
    ("set_locked", "set_locked", "locked"),
    ("set_ephemeral", "set_ephemeral", "expiration"),
    (
        "set_membership_approval",
        "set_membership_approval",
        "membership_approval",
    ),
    (
        "set_member_add_mode",
        "set_member_add_mode",
        "member_add_mode",
    ),
    ("set_limit_sharing", "set_limit_sharing", "limit_sharing"),
    ("set_group_history", "set_group_history", "group_history"),
    (
        "set_no_frequently_forwarded",
        "set_no_frequently_forwarded",
        "no_frequently_forwarded",
    ),
    (
        "set_allow_admin_reports",
        "set_allow_admin_reports",
        "allow_admin_reports",
    ),
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

#[test]
fn every_policy_action_reads_its_value_from_the_caller() {
    let compact = compact();
    for (action, method, param) in POLICY_ACTIONS {
        assert!(
            compact.contains(&format!("\"{action}\"=>")),
            "action '{action}' has no match arm: it would fall into the unknown-action error"
        );
        assert!(
            compact.contains(&format!("groups().{method}(")),
            "'{action}' does not call `client.groups().{method}`"
        );
        let reads_param = compact.contains(&format!("get_bool(&input,\"{param}\")"))
            || compact.contains(&format!("get_str(&input,\"{param}\")"))
            || compact.contains(&format!("input.get(\"{param}\")"));
        assert!(
            reads_param,
            "'{action}' never reads '{param}': a policy setter must take its value from the \
             caller rather than pick one"
        );
    }
}

#[test]
fn a_policy_flag_is_required_rather_than_defaulted() {
    assert_eq!(
        get_bool(&json!({"locked": true}), "locked").ok(),
        Some(true)
    );
    assert_eq!(
        get_bool(&json!({"locked": false}), "locked").ok(),
        Some(false)
    );
    assert!(
        get_bool(&json!({}), "locked").is_err(),
        "an absent flag must be refused: defaulting it flips a group setting silently"
    );
    assert!(
        get_bool(&json!({"locked": "yes"}), "locked").is_err(),
        "a non-boolean must be refused rather than coerced"
    );
}
