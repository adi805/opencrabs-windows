//! Chat-level state actions on the WhatsApp send tool: archive, mute, star,
//! clear, and saving a contact.
//!
//! The arms need a live socket, so the wire mutations cannot run here. What is
//! checkable without a connection is the part that decides what the model can
//! reach and which chat each action lands on: the dispatch table, the call
//! each arm makes, and the chat-vs-person target resolution.

use crate::brain::tools::whatsapp_send::resolve_chat_jid;
use serde_json::json;
use wacore_binary::jid::Server;

const SOURCE: &str = include_str!("../brain/tools/whatsapp_send.rs");

/// Every action this task added, paired with the call it must make.
///
/// The call is spelled with its receiver so the pin covers which feature the
/// arm reaches for: an archive answered by the mute path (or the reverse)
/// would still compile.
const ACTIONS: &[(&str, &str)] = &[
    ("archive_chat", "chat_actions().archive_chat("),
    ("unarchive_chat", "chat_actions().unarchive_chat("),
    ("mute_chat", "chat_actions().mute_chat("),
    ("unmute_chat", "chat_actions().unmute_chat("),
    ("star_message", "chat_actions().star_message("),
    ("unstar_message", "chat_actions().unstar_message("),
    ("clear_chat", "chat_actions().clear_chat("),
    ("save_contact", "chat_actions().save_contact("),
];

/// The text with all whitespace removed, so an assertion does not depend on
/// where rustfmt chose to wrap a chain across lines.
fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The body of the match arm that handles `action`: from its pattern to the
/// start of the next arm.
///
/// Pins are scoped to the arm on purpose. A file-wide `contains` would be
/// satisfied by a call living anywhere in a three-thousand-line file, so an
/// action whose arm was deleted while its helper stayed in use elsewhere would
/// still pass.
fn arm_body(action: &str) -> &'static str {
    let needle = format!("\"{action}\"");
    let mut from = 0;
    loop {
        let at = SOURCE[from..]
            .find(&needle)
            .map(|i| i + from)
            .unwrap_or_else(|| panic!("'{action}' does not appear in the source"));
        let after = &SOURCE[at + needle.len()..];
        // An arm pattern reaches `=> {` on its own line, right after the name:
        // a grouped arm such as `"a" | "b" => {` included. A hit further away,
        // or one that crosses a newline, is the schema enum or an error string.
        if let Some(rel) = after.find("=> {")
            && rel < 80
            && !after[..rel].contains('\n')
        {
            let rest = &after[rel..];
            let mut end = rest.len();
            for candidate in [
                rest.find("\n            \""),
                rest.find("\n            unknown"),
            ] {
                if let Some(i) = candidate {
                    end = end.min(i);
                }
            }
            return &rest[..end];
        }
        from = at + needle.len();
    }
}

#[test]
fn every_chat_action_has_a_match_arm_and_a_schema_entry() {
    let compact = squash(SOURCE);
    for (action, _) in ACTIONS {
        let occurrences = compact.matches(&format!("\"{action}\"")).count();
        assert!(
            occurrences >= 2,
            "'{action}' appears {occurrences} time(s): it needs both a schema enum entry \
             (so the model can discover it) and a match arm"
        );
    }
}

#[test]
fn the_chat_arms_call_the_feature_that_answers_them() {
    for (action, call) in ACTIONS {
        assert!(
            squash(arm_body(action)).contains(call),
            "'{action}' does not call `{call}`"
        );
    }
}

/// A mute that names a time must reach `mute_chat_until`, and one that does not
/// must reach `mute_chat`. Routing both to the indefinite call would silently
/// drop the expiry the caller asked for.
#[test]
fn mute_chat_routes_on_the_presence_of_an_expiry() {
    let body = squash(arm_body("mute_chat"));
    assert!(
        body.contains("chat_actions().mute_chat_until(&jid,until)"),
        "mute_chat must call mute_chat_until when an expiry is given"
    );
    assert!(
        body.contains("chat_actions().mute_chat(&jid)"),
        "mute_chat must call mute_chat when no expiry is given"
    );
}

/// The per-chat actions land on a CHAT (group or person), so they resolve
/// through `resolve_chat_jid`; `save_contact` names a PERSON and must not, or a
/// `group_jid` would be accepted as a contact id.
///
/// `resolve_chat_jid` is what makes a group reachable at all: without it the arm
/// takes the `resolve_jid` path, which does not read `group_jid`, so "archive
/// this group" would quietly archive the owner's own chat instead.
#[test]
fn chat_actions_target_a_chat_while_save_contact_targets_a_person() {
    for action in [
        "archive_chat",
        "unarchive_chat",
        "mute_chat",
        "unmute_chat",
        "star_message",
        "unstar_message",
        "clear_chat",
    ] {
        assert!(
            arm_body(action).contains("resolve_chat_jid("),
            "'{action}' must resolve its target through resolve_chat_jid"
        );
    }
    let save = arm_body("save_contact");
    assert!(
        save.contains("resolve_jid("),
        "save_contact must resolve a person, not a chat"
    );
    assert!(
        !save.contains("resolve_chat_jid("),
        "save_contact must not accept a `group_jid` as a contact id"
    );
}

/// A contact mutation with no name to write is a no-op on the wire, so the arm
/// refuses it rather than sending an empty contact.
#[test]
fn save_contact_refuses_when_no_name_is_given() {
    assert!(
        squash(arm_body("save_contact")).contains("full_name.is_none()&&first_name.is_none()"),
        "save_contact must refuse when neither name field is set"
    );
}

/// `resolve_jid` falls back to the session chat and then to the owner, which is
/// right for a message but wrong for a contact: saving a name has to name the
/// number it belongs to, so an omitted `phone` is a refusal here.
#[test]
fn save_contact_refuses_without_a_phone_number() {
    let body = squash(arm_body("save_contact"));
    assert!(
        body.contains("input.get(\"phone\").and_then(|v|v.as_str()).is_none()"),
        "save_contact must require an explicit 'phone' instead of falling back to the owner"
    );
}

/// `resolve_chat_jid` sends a `group_jid` through the group resolver and
/// everything else through the person resolver, so a bare group id cannot be
/// read as a phone number.
#[tokio::test]
async fn resolve_chat_jid_prefers_the_group_when_one_is_named() {
    let state = crate::channels::whatsapp::WhatsAppState::new();
    let (_tx, rx) = tokio::sync::watch::channel(crate::config::Config::default());

    let input = json!({ "group_jid": "120363012345678901" });
    let (jid, jid_str) = resolve_chat_jid(&input, &state, &rx, uuid::Uuid::nil())
        .await
        .expect("a bare group id must resolve");
    assert_eq!(jid.server, Server::Group);
    assert_eq!(jid_str, "120363012345678901@g.us");
}
