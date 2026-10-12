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
    (
        "delete_message_for_me",
        "chat_actions().delete_message_for_me(",
    ),
    ("delete_chat", "chat_actions().delete_chat("),
    ("mark_chat_as_read", "chat_actions().mark_chat_as_read("),
    // This one dispatches many ops, so the receiver is what is pinned.
    ("send_app_state_action", "client.chat_actions()"),
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
            let ends = [
                rest.find("\n            \""),
                rest.find("\n            unknown"),
            ];
            let end = ends.into_iter().flatten().min().unwrap_or(rest.len());
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
        "delete_message_for_me",
        "delete_chat",
        "mark_chat_as_read",
        "send_app_state_action",
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

/// A status mute names a PERSON, not a chat: it silences that contact's status
/// updates. Routing it through `resolve_chat_jid` would let a `group_jid` stand
/// in for the person being muted, so it keeps the user resolver.
#[test]
fn user_status_mute_targets_a_person_not_a_chat() {
    let body = arm_body("set_user_status_mute");
    assert!(
        body.contains("resolve_jid("),
        "set_user_status_mute must resolve a person through resolve_jid"
    );
    assert!(
        !body.contains("resolve_chat_jid("),
        "set_user_status_mute must not accept a `group_jid` as the person to mute"
    );
}

/// Every arm whose target is a person or the account itself, and so legitimately
/// takes the plain `resolve_jid` path.
///
/// This is the sweep's *allow-list*, not a set of cases to check. The test below
/// walks the whole dispatch table and fails on any arm that resolves a target
/// with the plain resolver without being listed here, so a new arm cannot
/// silently join the person-level set. It also fails on a listed arm that moved
/// to the chat resolver, so the list cannot go stale.
///
/// Why an allow-list and not a list of chat actions: the earlier version named
/// the arms it expected to be chat-level, and every arm nobody remembered to add
/// went unchecked. That is how `delete_chat`, `mark_chat_as_read` and
/// `send_app_state_action` shipped resolving the owner instead of the named
/// group. Inverting it makes the unchecked set empty by construction.
const PERSON_LEVEL: &[(&str, &str)] = &[
    ("block_contact", "blocks a contact, not a chat"),
    ("get_profile_picture", "reads a contact's picture"),
    (
        "get_profile_picture_with_timeout",
        "reads a contact's picture, with a caller-supplied timeout",
    ),
    ("is_blocked", "asks whether a contact is blocked"),
    ("save_contact", "writes a contact's address-book entry"),
    (
        "set_user_status_mute",
        "mutes a contact's status updates; the target is the person, not the chat",
    ),
    (
        "subscribe_presence,unsubscribe_presence",
        "watches a contact's presence",
    ),
    ("unblock_contact", "unblocks a contact, not a chat"),
    (
        "wa_privacy",
        "account privacy settings and the disallowed-contact list",
    ),
];

/// The header of a top-level match arm (`"name" => {` or `"a" | "b" => {`), with
/// the arm's leading indentation stripped.
///
/// Only arms at the dispatch table's own indentation are returned: the nested
/// matches inside `wa_introspect` and `send_app_state_action` sit deeper and are
/// part of their parent arm, not arms of their own.
fn top_level_arm_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("            ")?;
    if rest.starts_with(' ') {
        return None;
    }
    let head = rest.strip_suffix("=> {")?.trim_end();
    if !head.starts_with('"') {
        return None;
    }
    let quoted = head.split('|').all(|name| {
        let name = name.trim();
        name.len() > 2 && name.starts_with('"') && name.ends_with('"')
    });
    if !quoted {
        return None;
    }
    Some(head.to_string())
}

/// Split the dispatch table into `(header, body)` pairs, dropping comment lines
/// so a comment mentioning a resolver cannot decide which one an arm uses.
fn dispatch_arms() -> Vec<(String, String)> {
    let mut arms: Vec<(String, String)> = Vec::new();
    for line in SOURCE.lines() {
        if let Some(header) = top_level_arm_header(line) {
            arms.push((header, String::new()));
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if let Some((_, body)) = arms.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    arms
}

#[test]
fn every_target_taking_arm_uses_the_resolver_its_target_needs() {
    let arms = dispatch_arms();
    assert!(
        arms.len() > 100,
        "the dispatch table should have well over a hundred arms, found {}",
        arms.len()
    );

    let mut person_arms: Vec<String> = Vec::new();
    for (header, body) in &arms {
        let chat = body.contains("resolve_chat_jid(");
        let plain = body.contains("resolve_jid(");
        if !plain || chat {
            continue;
        }
        let key = header
            .split('|')
            .map(|name| name.trim().trim_matches('"'))
            .collect::<Vec<&str>>()
            .join(",");
        assert!(
            PERSON_LEVEL.iter().any(|(known, _)| *known == key),
            "arm '{header}' resolves its target with the plain resolver but is not declared in \
             PERSON_LEVEL. If the target is a chat, route it through resolve_chat_jid so \
             `group_jid` is honoured; if it really is a person, add it to PERSON_LEVEL with a \
             reason."
        );
        person_arms.push(key);
    }

    for (key, reason) in PERSON_LEVEL {
        assert!(
            !reason.trim().is_empty(),
            "PERSON_LEVEL entry '{key}' has no reason"
        );
        assert!(
            person_arms.iter().any(|seen| seen == key),
            "PERSON_LEVEL entry '{key}' is stale: no arm with that name still uses the plain \
             resolver"
        );
    }
    assert_eq!(person_arms.len(), PERSON_LEVEL.len());
}
