//! Ephemeral group replies: scoping, the request shape, and the edit and
//! delete methods that make them two-way (#756, #112).
//!
//! Covers the pure pieces: who a reply is scoped to, what each request body
//! looks like, and how a response's `ephemeral_message_id` is read. The
//! transport itself needs a live bot.

use crate::channels::telegram::ephemeral::{
    build_body, build_body_legacy, build_delete_body, build_edit_markup_body,
    build_edit_text_body, build_rich_body, ephemeral_id_from, forget_picker, picker_for,
    receiver_for, remember_picker,
};
use serde_json::json;
use teloxide::types::{MessageId, ThreadId};

// Each picker test uses its own chat id. The registry is process-global and the
// test harness runs these in parallel, so a shared id would let one test's
// forget step blank another's entry.
const PICKER_CHAT_A: i64 = -1_000_900_001;
const PICKER_CHAT_B: i64 = -1_000_900_002;
const PICKER_CHAT_C: i64 = -1_000_900_003;

#[test]
fn picker_round_trips_for_one_chat() {
    assert_eq!(picker_for(PICKER_CHAT_A), None, "test starts from no picker");
    assert_eq!(remember_picker(PICKER_CHAT_A, 41), None);
    assert_eq!(picker_for(PICKER_CHAT_A), Some(41));
    assert_eq!(forget_picker(PICKER_CHAT_A), Some(41));
    assert_eq!(picker_for(PICKER_CHAT_A), None);
}

#[test]
fn remember_picker_reports_the_bubble_it_replaced() {
    // The returned id is what the caller deletes: a second picker in the same
    // chat must supersede the first rather than leave two live keyboards whose
    // ids the registry has already forgotten.
    let _ = remember_picker(PICKER_CHAT_B, 7);
    assert_eq!(remember_picker(PICKER_CHAT_B, 8), Some(7));
    assert_eq!(picker_for(PICKER_CHAT_B), Some(8));
}

#[test]
fn remembering_the_same_picker_is_not_a_replacement() {
    // Re-sending the same id (a retry) must not tell the caller to delete the
    // message it just sent.
    let _ = remember_picker(PICKER_CHAT_C, 9);
    assert_eq!(remember_picker(PICKER_CHAT_C, 9), None);
    assert_eq!(picker_for(PICKER_CHAT_C), Some(9));
    let _ = forget_picker(PICKER_CHAT_C);
}

#[test]
fn dm_never_scopes_a_reply() {
    // A DM has nobody to hide the reply from, and asking for an untested
    // parameter there would risk the one path that already works.
    assert_eq!(receiver_for(true, 12345), None);
}

#[test]
fn group_scopes_the_reply_to_the_invoker() {
    assert_eq!(receiver_for(false, 12345), Some(12345));
}

#[test]
fn body_scopes_with_the_10_3_object() {
    // Bot API 10.3 replaced the flat parameter with this object, so the body
    // has to nest the receiver rather than set it at the top level.
    let body = build_body(-100200, None, 12345, "hello", false);
    assert_eq!(body["chat_id"], -100200);
    assert_eq!(body["text"], "hello");
    assert_eq!(body["ephemeral_message_parameters"]["receiver_user_id"], 12345);
    assert!(
        body.get("receiver_user_id").is_none(),
        "the flat parameter must not also be present, got {body}"
    );
}

#[test]
fn legacy_body_keeps_the_flat_receiver_user_id() {
    // The one-shot fallback for a server that predates the object.
    let body = build_body_legacy(-100200, None, 12345, "hello", false);
    assert_eq!(body["receiver_user_id"], 12345);
    assert!(
        body.get("ephemeral_message_parameters").is_none(),
        "the two shapes must not be mixed, got {body}"
    );
}

#[test]
fn both_shapes_carry_the_same_text_and_thread() {
    // The fallback must differ in scoping only. If it also dropped the thread
    // or the parse mode, a fallback send would land in the wrong topic.
    let current = build_body(-100200, Some(ThreadId(MessageId(77))), 12345, "<b>hi</b>", true);
    let legacy =
        build_body_legacy(-100200, Some(ThreadId(MessageId(77))), 12345, "<b>hi</b>", true);
    for key in ["chat_id", "text", "parse_mode", "message_thread_id"] {
        assert_eq!(current[key], legacy[key], "{key} drifted between the two shapes");
    }
}

#[test]
fn plain_body_has_no_parse_mode() {
    // Acks like "New session started." are literal text, and an HTML parse
    // mode would swallow any `<` or `&` they happen to contain.
    let body = build_body(-100200, None, 12345, "a < b & c", false);
    assert!(body.get("parse_mode").is_none());
}

#[test]
fn html_body_sets_parse_mode() {
    let body = build_body(-100200, None, 12345, "<b>hi</b>", true);
    assert_eq!(body["parse_mode"], "HTML");
}

#[test]
fn rich_body_is_the_public_body_plus_the_scoping_object() {
    // The scoped rich attempt must be the public rich request apart from the
    // scoping field, or the two paths render differently.
    let public = crate::channels::telegram::rich::api::build_body(-100200, None, "# hi", None);
    let scoped = build_rich_body(-100200, None, 12345, "# hi");
    assert_eq!(scoped["rich_message"], public["rich_message"]);
    assert_eq!(scoped["chat_id"], public["chat_id"]);
    assert_eq!(scoped["ephemeral_message_parameters"]["receiver_user_id"], 12345);
}

#[test]
fn rich_body_keeps_the_forum_topic() {
    let body = build_rich_body(-100200, Some(ThreadId(MessageId(77))), 12345, "# hi");
    assert_eq!(body["message_thread_id"], 77);
    assert_eq!(body["ephemeral_message_parameters"]["receiver_user_id"], 12345);
}

#[test]
fn edit_text_body_targets_the_ephemeral_id() {
    // `message_id` is 0 for an ephemeral message, so the ordinary edit method
    // cannot address one: the id has to be this field.
    let body = build_edit_text_body(-100200, 4242, "new text", false);
    assert_eq!(body["chat_id"], -100200);
    assert_eq!(body["ephemeral_message_id"], 4242);
    assert_eq!(body["text"], "new text");
    assert!(body.get("message_id").is_none(), "got {body}");
}

#[test]
fn edit_text_body_sets_parse_mode_only_when_asked() {
    assert_eq!(build_edit_text_body(-1, 7, "<b>x</b>", true)["parse_mode"], "HTML");
    assert!(build_edit_text_body(-1, 7, "a < b", false).get("parse_mode").is_none());
}

#[test]
fn edit_markup_body_carries_only_the_markup() {
    let markup = json!({"inline_keyboard": [[{"text": "x", "callback_data": "y"}]]});
    let body = build_edit_markup_body(-100200, 4242, &markup);
    assert_eq!(body["ephemeral_message_id"], 4242);
    assert_eq!(body["reply_markup"], markup);
    assert!(body.get("text").is_none(), "a markup edit must not resend text: {body}");
}

#[test]
fn delete_body_carries_chat_and_ephemeral_id() {
    let body = build_delete_body(-100200, 4242);
    assert_eq!(body["chat_id"], -100200);
    assert_eq!(body["ephemeral_message_id"], 4242);
    assert_eq!(body.as_object().map(serde_json::Map::len), Some(2));
}

#[test]
fn ephemeral_id_is_read_out_of_the_send_response() {
    let response = json!({"ok": true, "result": {"message_id": 0, "ephemeral_message_id": 4242}});
    assert_eq!(ephemeral_id_from(&response), Some(4242));
}

#[test]
fn ephemeral_id_absent_is_none_not_zero() {
    // A server that accepts the send but echoes no id must not be read as
    // "id 0", which would then be used to edit a message that does not exist.
    let no_field = json!({"ok": true, "result": {"message_id": 12}});
    assert_eq!(ephemeral_id_from(&no_field), None);
    let null_field = json!({"ok": true, "result": {"ephemeral_message_id": null}});
    assert_eq!(ephemeral_id_from(&null_field), None);
    let no_result = json!({"ok": true});
    assert_eq!(ephemeral_id_from(&no_result), None);
}

#[test]
fn thread_id_targets_the_forum_topic() {
    let body = build_body(-100200, Some(ThreadId(MessageId(77))), 12345, "hi", true);
    assert_eq!(body["message_thread_id"], 77);
}

#[test]
fn no_thread_id_omits_the_field() {
    // Sending `message_thread_id: null` to a non-forum chat is an API error,
    // so the field has to be absent rather than explicitly empty.
    let body = build_body(-100200, None, 12345, "hi", true);
    assert!(body.get("message_thread_id").is_none());
}
