//! Our own WhatsApp profile picture: setting one from a file, and removing it.
//!
//! The arms need a live socket, so the wire calls cannot run here. What is
//! checkable without a connection is the part that decides what the model can
//! reach and which call each action makes: the dispatch table, the argument
//! rules the arms refuse on, and that neither is charged to the send budget.

const SOURCE: &str = include_str!("../brain/tools/whatsapp_send.rs");

/// Every action this task added, paired with the call it must make.
///
/// The receiver is part of the pin: `set_profile_picture` also exists on a
/// group and on a contact, so a picture set through one of those would still
/// compile while changing someone else's account.
const ACTIONS: &[(&str, &str)] = &[
    ("set_profile_picture", "profile().set_profile_picture("),
    (
        "remove_profile_picture",
        "profile().remove_profile_picture(",
    ),
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
fn every_profile_picture_action_has_a_match_arm_and_a_schema_entry() {
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
fn the_profile_picture_arms_call_the_feature_that_answers_them() {
    for (action, call) in ACTIONS {
        assert!(
            squash(arm_body(action)).contains(call),
            "'{action}' does not call `{call}`"
        );
    }
}

/// The bytes go out as a JPEG `picture` node, so a PNG or a video is rejected
/// on the wire. Refusing here can name the file and the type it actually is,
/// which a wire rejection cannot.
#[test]
fn setting_a_picture_reads_a_local_file_and_refuses_a_non_jpeg() {
    let body = squash(arm_body("set_profile_picture"));
    assert!(
        body.contains("read_local_media(&path,\"image/jpeg\")"),
        "set_profile_picture must read the file as a JPEG"
    );
    assert!(
        body.contains("ifmime!=\"image/jpeg\""),
        "set_profile_picture must refuse a file that is not a JPEG"
    );
}

/// A removal names our own account in the IQ, so a `media_path` on it would be
/// read and then ignored, which reads back to the caller as a set that worked.
#[test]
fn removing_a_picture_takes_no_argument() {
    assert!(
        !squash(arm_body("remove_profile_picture")).contains("media_path"),
        "remove_profile_picture must not take a file"
    );
}

/// A picture is an IQ to the server, not a message to a chat, so it is not
/// charged to the send budget: doing so would spend a send allowance on a
/// stanza that reaches nobody.
#[test]
fn a_profile_picture_is_not_charged_to_the_send_budget() {
    for (action, _) in ACTIONS {
        assert!(
            !squash(arm_body(action)).contains("gate_send!"),
            "'{action}' sends no message and must not be charged to the send budget"
        );
    }
}
