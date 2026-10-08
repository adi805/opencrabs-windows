//! Bot API 10.3's `force_reply` field on the reply markups.
//!
//! 10.3 added an optional `force_reply` to `InlineKeyboardMarkup` and
//! `ReplyKeyboardMarkup`, and teloxide-core 0.13 has no binding for it, so the
//! field is stamped onto the serialized markup instead. These tests pin its
//! presence when asked for, its absence when not, and that the markup's own
//! fields survive the stamp.

use crate::channels::telegram::keyboards::markup_with_force_reply;
use serde_json::json;
use teloxide::types::{InlineKeyboardMarkup, KeyboardMarkup};

fn inline_markup() -> InlineKeyboardMarkup {
    let button = teloxide::types::InlineKeyboardButton::callback("Yes", "approve:1");
    InlineKeyboardMarkup::new(vec![vec![button]])
}

fn reply_markup() -> KeyboardMarkup {
    let button = teloxide::types::KeyboardButton::new("Yes");
    KeyboardMarkup::new(vec![vec![button]])
}

#[test]
fn inline_keyboard_carries_force_reply_when_asked() {
    let markup = inline_markup();
    let body = markup_with_force_reply(&markup, true).expect("inline markup serializes");
    assert_eq!(body["force_reply"], json!(true));
}

#[test]
fn inline_keyboard_omits_force_reply_unless_asked() {
    let markup = inline_markup();
    let body = markup_with_force_reply(&markup, false).expect("inline markup serializes");
    assert!(body.get("force_reply").is_none());
}

#[test]
fn reply_keyboard_carries_force_reply_when_asked() {
    let markup = reply_markup();
    let body = markup_with_force_reply(&markup, true).expect("reply markup serializes");
    assert_eq!(body["force_reply"], json!(true));
}

#[test]
fn reply_keyboard_omits_force_reply_unless_asked() {
    let markup = reply_markup();
    let body = markup_with_force_reply(&markup, false).expect("reply markup serializes");
    assert!(body.get("force_reply").is_none());
}

#[test]
fn the_field_lands_in_the_send_body_under_reply_markup() {
    let reply_markup = markup_with_force_reply(&inline_markup(), true).expect("serializes");
    let body = json!({
        "chat_id": -100200,
        "text": "hi",
        "reply_markup": reply_markup,
    });
    assert_eq!(body["reply_markup"]["force_reply"], json!(true));
    assert_eq!(
        body["reply_markup"]["inline_keyboard"][0][0]["text"],
        json!("Yes")
    );
}

#[test]
fn stamping_keeps_the_markups_own_fields() {
    let inline = inline_markup();
    let body = markup_with_force_reply(&inline, true).expect("inline markup serializes");
    let rows = body["inline_keyboard"].as_array().expect("rows survive");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0]["text"], json!("Yes"));

    let reply = reply_markup();
    let body = markup_with_force_reply(&reply, true).expect("reply markup serializes");
    let keys = body["keyboard"].as_array().expect("rows survive");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0][0]["text"], json!("Yes"));
}
