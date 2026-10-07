//! Service-event content keys must never be synthesized into agent text (#110).
//!
//! `synthesize_unknown_content` rewrites a message whose content keys are all
//! unknown into a plain text message so it flows through the whole pipeline.
//! That is right for content this client cannot decode (#359), and wrong for
//! service events: a community join or a paid-media purchase is not something
//! the user said. The keys below are service events the Bot API shipped after
//! teloxide-core 0.13's serde definitions, so they belong in
//! `KNOWN_CONTENT_KEYS` next to pins, joins and topic events.

use crate::channels::telegram::raw_updates::synthesize_unknown_content;
use serde_json::{json, Value};

/// A message shaped like a real update: envelope plus exactly one content key.
fn message_with(key: &str, payload: Value) -> Value {
    let mut m = json!({
        "message_id": 10,
        "date": 1_750_000_000_i64,
        "chat": {"id": -100200, "type": "supergroup", "title": "T"},
        "from": {"id": 123, "is_bot": false, "first_name": "A"},
    });
    if let Some(obj) = m.as_object_mut() {
        obj.insert(key.to_string(), payload);
    }
    m
}

/// The rewrite must not fire and the service key must survive untouched.
fn assert_untouched(key: &str) {
    let mut m = message_with(key, json!({"payload": "x"}));
    synthesize_unknown_content(&mut m);
    assert!(
        m.get("text").is_none(),
        "{key} is a service event: it must not be rewritten into agent text, got {m}"
    );
    assert!(m.get(key).is_some(), "{key} must survive untouched, got {m}");
}

#[test]
fn community_chat_joined_is_not_synthesized_into_text() {
    assert_untouched("community_chat_joined");
}

#[test]
fn community_chat_removed_is_not_synthesized_into_text() {
    assert_untouched("community_chat_removed");
}

#[test]
fn purchased_paid_media_is_not_synthesized_into_text() {
    assert_untouched("purchased_paid_media");
}

/// Negative control. Without it a guard that never fires, or a rewrite that
/// silently became a no-op, would pass every assertion above.
#[test]
fn genuinely_unknown_content_is_still_synthesized() {
    let mut m = message_with("brand_new_content_key_from_the_future", json!({"a": 1}));
    synthesize_unknown_content(&mut m);
    assert!(m.get("text").is_some(), "unknown content must still be synthesized");
    assert!(
        m.get("brand_new_content_key_from_the_future").is_none(),
        "the unknown key is dropped once it is rewritten, got {m}"
    );
}

/// Near miss from another shape: an envelope-only message carries nothing to
/// rewrite, so the guard must leave it alone rather than invent content.
#[test]
fn envelope_only_message_is_left_alone() {
    let mut m = json!({
        "message_id": 11,
        "date": 1_750_000_000_i64,
        "chat": {"id": -100200, "type": "supergroup", "title": "T"},
    });
    synthesize_unknown_content(&mut m);
    assert!(m.get("text").is_none(), "nothing to synthesize, got {m}");
}
