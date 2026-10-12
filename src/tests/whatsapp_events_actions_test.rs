//! Event (RSVP) and channel-comment actions on the WhatsApp send tool.
//!
//! The arms need a live socket, so the wire calls cannot run here. What is
//! checkable without a connection is what the model can reach and which call
//! each action makes: the dispatch table, the argument rules the arms refuse
//! on, and the fact that a writer is charged to the shared send budget.

const SOURCE: &str = include_str!("../brain/tools/whatsapp_send.rs");

/// Every action this task added, paired with the call it must make.
///
/// The receiver is part of the pin: an RSVP sent through the event surface but
/// keyed as a plain message, or a comment posted through the channel surface
/// instead of the comment surface, would still compile.
const ACTIONS: &[(&str, &str)] = &[
    ("create_event", "events().create("),
    ("respond_event", "events().respond("),
    ("post_comment", "comments().send_text("),
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
fn every_action_has_a_match_arm_and_a_schema_entry() {
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
fn each_arm_calls_the_feature_that_answers_it() {
    for (action, call) in ACTIONS {
        assert!(
            squash(arm_body(action)).contains(call),
            "'{action}' does not call `{call}`"
        );
    }
}

/// Creating an event, answering one, and commenting all put a message on the
/// wire, so each is charged to the same send budget as any other outbound act.
#[test]
fn every_new_action_is_charged_to_the_send_budget() {
    for (action, _) in ACTIONS {
        assert!(
            squash(arm_body(action)).contains("gate_send!"),
            "'{action}' posts to WhatsApp and must be charged to the send budget"
        );
    }
}

/// An RSVP's key is derived from the secret minted when the event was created,
/// and the secret cannot be recovered from the event afterwards. An arm that
/// created the event without reporting the secret would leave every answer
/// unreadable by construction, which is exactly the shape of the poll bug this
/// suite already paid for once.
#[test]
fn creating_an_event_reports_the_secret_that_decrypts_its_rsvps() {
    let body = squash(arm_body("create_event"));
    assert!(
        body.contains("STANDARD.encode(&secret)"),
        "create_event must report the event secret in a form the caller can keep"
    );
    assert!(
        body.contains("result.message_id"),
        "create_event must report the event message id, which respond_event needs"
    );
}

/// The RSVP cannot be encrypted without the event's secret, so the arm reads it
/// from the caller rather than inventing one. The lib rejects a wrong length
/// instead of sending something undecryptable.
#[test]
fn answering_an_event_reads_the_secret_it_was_given() {
    let body = squash(arm_body("respond_event"));
    assert!(
        body.contains("decode_b64(&input,\"event_secret\")"),
        "respond_event must decode the base64 event secret"
    );
    for variant in [
        "EventResponseType::GOING",
        "EventResponseType::NOT_GOING",
        "EventResponseType::MAYBE",
    ] {
        assert!(
            body.contains(variant),
            "respond_event must map an answer onto {variant}"
        );
    }
}

/// A typo in the answer must not quietly become a "maybe": the arm names the
/// three spellings it accepts instead of defaulting to one.
#[test]
fn an_unknown_answer_is_refused() {
    assert!(
        squash(arm_body("respond_event")).contains("Invalid'event_response'value"),
        "respond_event must refuse an answer it does not recognise"
    );
}

/// A comment is encrypted under the parent post's key, so the post author has
/// to travel with the post id or receivers cannot derive the same key and the
/// comment arrives unreadable.
#[test]
fn a_comment_carries_the_parent_post_key() {
    let body = squash(arm_body("post_comment"));
    assert!(
        body.contains("id:Some(parent_id.clone())"),
        "post_comment must carry the parent post id"
    );
    assert!(
        body.contains("participant:author.map(str::to_string)"),
        "post_comment must carry the post author in the parent key"
    );
}

/// The comment target is a chat, and the arm resolves it the way every other
/// chat-scoped action does. Reading it as a newsletter would point the comment
/// at a channel id that no post belongs to.
#[test]
fn a_comment_targets_a_chat() {
    assert!(
        squash(arm_body("post_comment")).contains("resolve_group_jid(&input)"),
        "post_comment must resolve its target as a chat"
    );
}
