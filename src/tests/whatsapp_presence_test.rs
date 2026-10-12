//! Presence on the WhatsApp send tool: our own online state and the
//! subscriptions that watch a contact.
//!
//! Both halves need a live socket, so the stanzas cannot be sent here. What is
//! checkable is the routing: which feature each arm reaches for, which arm is
//! allowed to resolve a target at all, and that a presence update is not
//! charged to the message budget the way a send is.

const SOURCE: &str = include_str!("../brain/tools/whatsapp_send.rs");

/// Every action this task added, paired with the call it must make.
///
/// The call is spelled with its receiver so the pin covers which feature the
/// arm reaches for: an unsubscribe answered by the subscribe path (or the
/// reverse) would still compile and still read as success.
const ACTIONS: &[(&str, &str)] = &[
    ("set_available", "presence().set_available("),
    ("set_unavailable", "presence().set_unavailable("),
    ("subscribe_presence", "presence().subscribe("),
    ("unsubscribe_presence", "presence().unsubscribe("),
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
fn every_presence_action_has_a_match_arm_and_a_schema_entry() {
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
fn the_presence_arms_call_the_feature_that_answers_them() {
    for (action, call) in ACTIONS {
        assert!(
            squash(arm_body(action)).contains(call),
            "'{action}' does not call `{call}`"
        );
    }
}

/// Setting our own presence names no target: it is about this session, not
/// about a contact. An arm that resolved a JID here would let a `phone`
/// argument silently retarget the call, and the success message would still
/// read as if the session's own state had changed.
#[test]
fn setting_our_own_presence_resolves_no_target() {
    for action in ["set_available", "set_unavailable"] {
        let body = squash(arm_body(action));
        assert!(
            !body.contains("resolve_jid(") && !body.contains("resolve_chat_jid("),
            "'{action}' is about our own state and must not resolve a JID"
        );
    }
}

/// A subscription names the contact whose updates we want, so it has to name
/// one. The `resolve_jid` fallback would otherwise pick the session chat and
/// then the owner, subscribing to whoever happens to be talking instead of the
/// contact the caller asked about.
#[test]
fn presence_subscriptions_require_an_explicit_contact() {
    for action in ["subscribe_presence", "unsubscribe_presence"] {
        let body = squash(arm_body(action));
        assert!(
            body.contains("input.get(\"phone\").and_then(|v|v.as_str()).is_none()"),
            "'{action}' must require an explicit 'phone' instead of falling back"
        );
        assert!(
            body.contains("resolve_jid(") && !body.contains("resolve_chat_jid("),
            "'{action}' watches a person, so it must resolve through resolve_jid"
        );
    }
}

/// A presence stanza is not a message, so it is not charged to the send budget
/// the way a send is. Gating it would make going offline fail on a rate limit
/// the presence update did not consume.
#[test]
fn presence_is_not_charged_to_the_send_budget() {
    for (action, _) in ACTIONS {
        assert!(
            !squash(arm_body(action)).contains("gate_send!"),
            "'{action}' must not be gated as a send: presence is not a message"
        );
    }
}
