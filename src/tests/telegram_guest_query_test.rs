//! Guest mode (#2087): answering an `@botname` tag in a chat the bot is not a
//! member of.
//!
//! Bot API 10.0 added `Update.guest_message`, `Message.guest_query_id`, the
//! caller fields and the method `answerGuestQuery`; the pinned
//! teloxide-core 0.13.0 knows none of them, so a guest update reaches the
//! typed dispatcher as `UpdateKind::Error` and the raw update path
//! (`channels/telegram/raw_updates.rs` -> `channels/telegram/guest.rs`) is the
//! only way in. Covered here are the four required cases: the parse, the
//! not-a-guest case, the `answerGuestQuery` body (including the 4096 clamp
//! Telegram imposes on the message text), and the caller gate.

use serde_json::json;

use crate::channels::telegram::guest::{answer_guest_query_body, parse_guest_query};
use crate::config::types::{TelegramConfig, TelegramGroupConfig};

/// A guest update as Telegram delivers it: someone tagged the bot in a chat
/// the bot is not a member of.
fn guest_update() -> serde_json::Value {
    json!({
        "update_id": 77,
        "guest_message": {
            "message_id": 5,
            "date": 1_750_000_000,
            "guest_query_id": "gq-abc123",
            "guest_bot_caller_user": {
                "id": 4242,
                "is_bot": false,
                "first_name": "Ana",
                "username": "ana"
            },
            "guest_bot_caller_chat": {
                "id": -100999,
                "type": "supergroup",
                "title": "Ana's crew"
            },
            "text": "@opencrabs what is 2+2?"
        }
    })
}

fn telegram_cfg(
    guest_queries: bool,
    allowed: &[&str],
    owner: &[&str],
    groups: &[(&str, &[&str], bool)],
) -> TelegramConfig {
    let mut g = std::collections::HashMap::new();
    for (id, users, open) in groups {
        g.insert(
            (*id).to_string(),
            TelegramGroupConfig {
                allowed_users: users.iter().map(|s| s.to_string()).collect(),
                open: *open,
                ..Default::default()
            },
        );
    }
    TelegramConfig {
        guest_queries,
        allowed_users: allowed.iter().map(|s| s.to_string()).collect(),
        bot_owner: owner.iter().map(|s| s.to_string()).collect(),
        groups: g,
        ..Default::default()
    }
}

#[test]
fn guest_message_with_query_id_parses_and_the_typed_parse_cannot_see_it() {
    let q = parse_guest_query(&guest_update()).expect("a guest update parses");
    assert_eq!(q.query_id, "gq-abc123");
    assert_eq!(q.user_id, 4242);
    assert_eq!(q.username.as_deref(), Some("ana"));
    assert_eq!(q.chat_id, Some(-100999));
    assert_eq!(q.chat_title.as_deref(), Some("Ana's crew"));
    assert_eq!(q.text, "@opencrabs what is 2+2?");

    // The typed deserializer has no `guest_message` variant, so the same
    // payload arrives as UpdateKind::Error and no dispatcher branch matches
    // it. This is why the raw branch exists; if teloxide grows the variant,
    // this assertion fails and the raw branch can be simplified.
    let typed: teloxide::types::Update =
        serde_json::from_str(&guest_update().to_string()).expect("lenient parse");
    assert!(
        matches!(typed.kind, teloxide::types::UpdateKind::Error(_)),
        "teloxide parsed a guest update into a typed kind"
    );
}

#[test]
fn a_message_without_a_query_id_does_not_take_the_guest_path() {
    // Guest message, but nothing to answer into.
    let mut no_id = guest_update();
    no_id["guest_message"]
        .as_object_mut()
        .expect("guest_message is an object")
        .remove("guest_query_id");
    assert!(
        parse_guest_query(&no_id).is_none(),
        "an empty guest_query_id is not answerable"
    );

    // A guest message with no identifiable caller: the gate would have
    // nothing to check, so it is dropped rather than answered on faith.
    let mut no_caller = guest_update();
    no_caller["guest_message"]
        .as_object_mut()
        .expect("guest_message is an object")
        .remove("guest_bot_caller_user");
    assert!(
        parse_guest_query(&no_caller).is_none(),
        "no caller means no gate to apply"
    );

    // An ordinary DM is not a guest query.
    let dm = json!({
        "update_id": 1,
        "message": {
            "message_id": 10,
            "date": 1_750_000_000,
            "chat": {"id": 123, "type": "private", "first_name": "A"},
            "from": {"id": 123, "is_bot": false, "first_name": "A"},
            "text": "hello"
        }
    });
    assert!(
        parse_guest_query(&dm).is_none(),
        "a DM is not a guest query"
    );
}

#[test]
fn answer_body_is_a_wellformed_guest_result_and_honours_the_text_ceiling() {
    let body = answer_guest_query_body("gq-1", "Hello there\nsecond line");
    assert_eq!(body["guest_query_id"], json!("gq-1"));
    assert_eq!(body["result"]["type"], json!("article"));
    assert_eq!(body["result"]["id"], json!("opencrabs-guest"));
    assert_eq!(body["result"]["title"], json!("Hello there"));
    assert_eq!(
        body["result"]["input_message_content"]["message_text"],
        json!("Hello there\nsecond line")
    );
    assert!(
        body["result"]["input_message_content"]
            .get("parse_mode")
            .is_none(),
        "guest replies go out as plain text"
    );
    let id = body["result"]["id"]
        .as_str()
        .expect("result id is a string");
    assert!(id.len() <= 64, "inline result id over Telegram's cap: {id}");

    // Telegram rejects message_text past 4096; long replies get cut, not lost.
    let long = "x".repeat(5000);
    let clamped = answer_guest_query_body("gq-2", &long);
    let text = clamped["result"]["input_message_content"]["message_text"]
        .as_str()
        .expect("message_text is a string");
    assert_eq!(text.chars().count(), 4096, "clamped to the ceiling");
    assert!(text.ends_with("[truncated]"), "cut is marked: {text:?}");
    assert!(text.starts_with(&"x".repeat(64)));

    // The cut counts characters, so multi-byte text is never split in half.
    let emoji = "\u{1F980}".repeat(5000);
    let multibyte = answer_guest_query_body("gq-3", &emoji);
    let text = multibyte["result"]["input_message_content"]["message_text"]
        .as_str()
        .expect("message_text is a string");
    assert_eq!(text.chars().count(), 4096);
    assert!(text.starts_with('\u{1F980}'));
}

#[test]
fn the_caller_gate_refuses_unlisted_callers_and_grants_nothing_when_off() {
    let groups: &[(&str, &[&str], bool)] = &[("-100g", &["222"], false), ("-100open", &[], true)];

    // Default is off: not even the owner is answered.
    let off = telegram_cfg(false, &["111"], &["111"], groups);
    assert!(!off.guest_allowed("111", Some("-100g")));
    assert!(!off.guest_allowed("111", None));

    let on = telegram_cfg(true, &["111"], &["111"], groups);
    // Owner and admins are answered anywhere, caller chat or not.
    assert!(on.guest_allowed("111", None));
    assert!(on.guest_allowed("111", Some("-100nowhere")));
    // A listed user is answered in their own chat only.
    assert!(on.guest_allowed("222", Some("-100g")));
    assert!(
        !on.guest_allowed("222", Some("-100other")),
        "a listing is per chat"
    );
    assert!(
        !on.guest_allowed("222", None),
        "no caller chat means nothing to check"
    );
    // Unlisted caller: refused, even in a chat the bot knows.
    assert!(!on.guest_allowed("333", Some("-100g")));
    // A group's open = true opens THAT group to the bot; it does not mean the
    // bot answers guest tags from its members.
    assert!(!on.guest_allowed("444", Some("-100open")));
}
