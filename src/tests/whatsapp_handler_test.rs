//! Tests for WhatsApp handler: `split_message`, `extract_text`, `has_image`.

use crate::channels::whatsapp::handler::{extract_text, has_image, split_message};
use waproto::whatsapp::Message;

// ── split_message ─────────────────────────────────────────────────────

#[test]
fn split_short_message() {
    let chunks = split_message("hello", 4000);
    assert_eq!(chunks, vec!["hello"]);
}

#[test]
fn split_long_message() {
    let text = "a\n".repeat(3000);
    let chunks = split_message(&text, 4000);
    assert!(chunks.len() >= 2);
    for chunk in &chunks {
        assert!(chunk.len() <= 4000);
    }
    let joined: String = chunks.into_iter().collect();
    assert_eq!(joined, text);
}

// ── extract_text ──────────────────────────────────────────────────────

#[test]
fn extract_text_conversation() {
    let msg = Message {
        conversation: Some("hello".to_string()),
        ..Default::default()
    };
    assert_eq!(extract_text(&msg), Some("hello".to_string()));
}

#[test]
fn extract_text_image_caption() {
    let msg = Message {
        image_message: waproto::whatsapp::message::ImageMessage {
            caption: Some("look at this".to_string()),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    assert_eq!(extract_text(&msg), Some("look at this".to_string()));
}

// ── has_image ─────────────────────────────────────────────────────────

#[test]
fn has_image_text_msg() {
    let msg = Message {
        conversation: Some("hi".to_string()),
        ..Default::default()
    };
    assert!(!has_image(&msg));
}

#[test]
fn has_image_img_msg() {
    let msg = Message {
        image_message: waproto::whatsapp::message::ImageMessage::default().into(),
        ..Default::default()
    };
    assert!(has_image(&msg));
}

// ── status updates ────────────────────────────────────────────────────

/// A status update is addressed to the `status@broadcast` pseudo-chat, which
/// cannot receive a message. Without an ingest guard the handler starts a full
/// turn for it and every send fails with `send_message to status@broadcast
/// requires reaction_message.key.participant = status author`, once per
/// composing tick plus once for the self-healing alert.
///
/// This is a shape test rather than a unit test on purpose: the guard lives at
/// the top of the inbound handler, and nothing in this suite can build the
/// client and state that handler needs. Asserting the guard is still there, and
/// still ahead of the first call that writes into the chat, is the only check
/// that fails when someone deletes it.
#[test]
fn status_broadcast_is_dropped_at_ingest() {
    let src = include_str!("../channels/whatsapp/handler.rs");

    let guard = src.find("is_status_broadcast()").expect(
        "the inbound handler must drop status@broadcast: answering a status \
         cannot succeed and costs a whole turn plus an error storm",
    );

    // `send_composing` writes into `info.source.chat`, so the guard has to run
    // before the first one. Any later and the composing retry loop starts
    // before the chat is known to be unreachable.
    let first_write = src
        .find("send_composing")
        .expect("the handler is expected to signal composing somewhere");
    assert!(
        guard < first_write,
        "the status@broadcast guard must run before the first write into the chat"
    );
}
