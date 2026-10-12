//! Contact queries on the WhatsApp send tool: profile pictures, existence
//! checks, and user info.
//!
//! The arms themselves need a live socket, so the wire calls cannot be
//! exercised here. What IS checkable without a connection is the part that
//! decides what the model can reach and who it is allowed to ask about: the
//! dispatch table, the call each arm makes, and the array parsing that turns
//! caller input into JIDs.

use crate::brain::tools::whatsapp_send::{
    get_bool, resolve_contacts, resolve_group_jids, resolve_participants,
};
use serde_json::json;
use wacore_binary::jid::Server;

const SOURCE: &str = include_str!("../brain/tools/whatsapp_send.rs");

/// Every action this task added, paired with the call it must make.
///
/// The call is spelled with its receiver so the pin covers which feature the
/// arm reaches for, not merely the method name: a contact query answered by the
/// group feature (or the reverse) would still compile.
const ACTIONS: &[(&str, &str)] = &[
    ("get_profile_picture", "contacts().get_profile_picture("),
    (
        "get_group_profile_picture",
        "groups().get_profile_pictures(",
    ),
    ("is_on_whatsapp", "contacts().is_on_whatsapp("),
    ("get_user_info", "contacts().get_user_info("),
];

/// The source with all whitespace removed, so an assertion does not depend on
/// where rustfmt chose to wrap a chain across lines.
fn compact() -> String {
    SOURCE.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn every_contact_action_has_a_match_arm_and_a_schema_entry() {
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
fn the_contact_arms_call_the_feature_that_answers_them() {
    let compact = compact();
    for (action, call) in ACTIONS {
        assert!(compact.contains(call), "'{action}' does not call `{call}`");
    }
}

/// A contact query has no default target. Both the person-level and group-level
/// lookups must resolve what the caller named, so the four arms are pinned to
/// the helper that reads the input rather than to anything the session carries.
#[test]
fn the_contact_queries_never_invent_a_target() {
    let compact = compact();
    assert!(
        compact.contains("\"get_profile_picture\"=>{let(jid,jid_str)=pget!(resolve_jid("),
        "get_profile_picture must resolve the person it is asked about"
    );
    for action in ["is_on_whatsapp", "get_user_info"] {
        assert!(
            compact.contains(&format!(
                "\"{action}\"=>{{letjids=pget!(resolve_contacts(&input));"
            )),
            "'{action}' must read the `contacts` array rather than fall back to a session JID"
        );
    }
    assert!(
        compact
            .contains("\"get_group_profile_picture\"=>{letjids=pget!(resolve_group_jids(&input));"),
        "get_group_profile_picture must read the `group_jids` array"
    );
}

/// The preview flag is optional, but only its absence may be defaulted: a
/// present-but-unusable value has to be refused, because silently picking a size
/// returns a different picture than the caller asked for.
#[test]
fn a_preview_flag_is_optional_but_never_silently_reinterpreted() {
    let compact = compact();
    assert_eq!(
        compact.matches("input.get(\"preview\")").count(),
        3,
        "all three picture actions must treat 'preview' as optional"
    );
    assert_eq!(
        compact.matches("get_bool(&input,\"preview\")").count(),
        3,
        "all three picture actions must refuse a non-boolean 'preview' rather than coerce it"
    );
    assert_eq!(
        get_bool(&json!({"preview": true}), "preview").ok(),
        Some(true)
    );
    assert_eq!(
        get_bool(&json!({"preview": false}), "preview").ok(),
        Some(false)
    );
    assert!(
        get_bool(&json!({"preview": "yes"}), "preview").is_err(),
        "a non-boolean must be refused rather than coerced"
    );
}

/// Both array parsers are shared by the person-level actions, so the two
/// spellings cannot drift: the same input must produce the same JIDs whichever
/// key it arrived under.
#[test]
fn contacts_and_participants_normalize_the_same_way() {
    let input = json!([
        " +15551234567",
        "15559876543@s.whatsapp.net",
        "15550001111:12"
    ]);
    let via_contacts = resolve_contacts(&json!({"contacts": input})).expect("contacts parse");
    let via_participants =
        resolve_participants(&json!({"participants": input})).expect("participants parse");
    assert_eq!(
        via_contacts, via_participants,
        "the shared parser must not normalize the two keys differently"
    );

    let users: Vec<&str> = via_contacts.iter().map(|j| j.user.as_str()).collect();
    assert_eq!(
        users,
        vec!["15551234567", "15559876543", "15550001111"],
        "a leading '+' and a ':device' suffix are not part of the address"
    );
}

#[test]
fn a_contact_query_that_names_nobody_is_refused() {
    assert!(
        resolve_contacts(&json!({})).is_err(),
        "an absent contact list is refused"
    );
    assert!(
        resolve_contacts(&json!({"contacts": []})).is_err(),
        "an empty list is refused rather than fanned out across the account"
    );
    assert!(
        resolve_contacts(&json!({"contacts": ["   "]})).is_err(),
        "a blank entry is refused"
    );
    assert!(
        resolve_contacts(&json!({"participants": ["15551234567"]})).is_err(),
        "the contacts key is required: reading 'participants' here would answer a different \
         question than the caller asked"
    );
}

#[test]
fn group_jids_accept_bare_ids_and_full_jids_and_refuse_junk() {
    let parsed = resolve_group_jids(&json!({
        "group_jids": ["120363012345678901", "120363012345678902@g.us"]
    }))
    .expect("bare ids and full JIDs parse");
    assert_eq!(parsed.len(), 2);
    for jid in &parsed {
        assert_eq!(jid.server, Server::Group, "{jid} is not a group JID");
    }

    let bare = resolve_group_jids(&json!({"group_jids": ["120363012345678901"]}))
        .expect("a bare group id is accepted");
    let full = resolve_group_jids(&json!({"group_jids": ["120363012345678901@g.us"]}))
        .expect("a full group JID is accepted");
    assert_eq!(bare, full, "both spellings name the same group");

    assert!(
        resolve_group_jids(&json!({})).is_err(),
        "an absent list is refused"
    );
    assert!(
        resolve_group_jids(&json!({"group_jids": []})).is_err(),
        "an empty list is refused rather than reported as a no-op"
    );
    assert!(
        resolve_group_jids(&json!({"group_jids": ["  "]})).is_err(),
        "a blank entry is refused"
    );
    assert!(
        resolve_group_jids(&json!({"group_jids": ["15551234567@s.whatsapp.net"]})).is_err(),
        "a phone JID is not a group: the lookup must not accept a person where a group is asked"
    );
}
