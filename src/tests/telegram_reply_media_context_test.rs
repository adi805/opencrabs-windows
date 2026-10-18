//! #2121: a reply to a media message carries the media identity, not just its
//! caption.
//!
//! The reply context was built from `reply.text().or(reply.caption())`, and a
//! media message has no `text()`: the context became the caption and the media
//! handle was dropped. The exact-id recovery lookup that could have supplied a
//! path was gated behind `full_text.is_empty()`, which a captioned photo is
//! not, and the media row was stored twice under one platform id with the
//! pathless `[photo] <caption>` twin winning the `ORDER BY created_at DESC`.
//!
//! Everything pinned here is pure: kind detection, path extraction, the note
//! text, and the context composition. The SQL ordering has its own check in
//! `channel_message_attachment_preference_test`.

use crate::channels::telegram::handler::{
    attachment_path_in, compose_reply_context, media_kind_of, media_reference_note,
    strip_attachment_tag,
};
use teloxide::types::{ChatId, Message, MessageId};

/// A message with no media at all: plain text.
fn text_message() -> Message {
    serde_json::from_value(serde_json::json!({
        "message_id": 7,
        "date": 1792249530,
        "chat": {"id": -1004367145331i64, "type": "supergroup", "title": "t"},
        "from": {"id": 42, "is_bot": false, "first_name": "Adi"},
        "text": "halo"
    }))
    .expect("a plain text message")
}

/// A photo message carrying `caption` (empty means captionless).
fn photo_message(caption: &str) -> Message {
    let mut value = serde_json::json!({
        "message_id": 91,
        "date": 1792249530,
        "chat": {"id": -1004367145331i64, "type": "supergroup", "title": "t"},
        "from": {"id": 42, "is_bot": false, "first_name": "Adi"},
        "photo": [{
            "file_id": "AgACAgIAAxkBAAI",
            "file_unique_id": "abc",
            "width": 1080,
            "height": 1080,
            "file_size": 1024
        }]
    });
    if !caption.is_empty() {
        value["caption"] = serde_json::Value::String(caption.to_string());
    }
    serde_json::from_value(value).expect("a photo message")
}

#[test]
fn a_plain_text_message_has_no_media_kind() {
    assert_eq!(
        media_kind_of(&text_message()),
        None,
        "a text message must not be reported as media"
    );
}

#[test]
fn a_photo_is_named_even_when_it_carries_a_caption() {
    // The caption is what used to be mistaken for the whole message.
    assert_eq!(
        media_kind_of(&photo_message("toolkuy-avatar-1080.png")),
        Some("photo")
    );
    assert_eq!(media_kind_of(&photo_message("")), Some("photo"));
}

#[test]
fn the_attachment_path_is_read_out_of_the_stored_row() {
    let row =
        "Ini wajah gua bro\n[file: /root/.opencrabs/channel_attachments/telegram/photo_91.jpg]";
    assert_eq!(
        attachment_path_in(row),
        Some("/root/.opencrabs/channel_attachments/telegram/photo_91.jpg"),
        "the path is what identifies the artifact"
    );
}

#[test]
fn a_row_without_an_attachment_yields_no_path() {
    for content in [
        "Ini wajah gua bro",
        "[photo] Ini wajah gua bro",
        "[photo] ",
        "[file: ]",
        "",
    ] {
        assert_eq!(
            attachment_path_in(content),
            None,
            "a row with no usable path must not pretend to carry one: {content:?}"
        );
    }
}

#[test]
fn a_resolved_photo_becomes_an_image_marker() {
    let note = media_reference_note("photo", Some("/tmp/photo_91.jpg"));
    assert!(
        note.contains("<<IMG:/tmp/photo_91.jpg>>"),
        "the inbound pipeline inlines images from this marker: {note}"
    );
    assert!(note.contains("photo"), "the kind must be named: {note}");
}

#[test]
fn other_media_kinds_get_their_path() {
    let note = media_reference_note("document", Some("/tmp/report.pdf"));
    assert!(note.contains("/tmp/report.pdf"), "{note}");
    assert!(!note.contains("<<IMG:"), "only photos inline: {note}");
    assert!(note.contains("document"), "{note}");
}

#[test]
fn unresolved_media_says_so_instead_of_falling_back_to_the_caption() {
    let note = media_reference_note("photo", None);
    assert!(
        note.contains("could not be resolved"),
        "an unresolvable media target must be stated, not guessed at: {note}"
    );
    assert!(
        note.contains("Do not guess"),
        "the instruction has to be explicit: {note}"
    );
}

#[test]
fn the_media_note_is_appended_to_the_reply_context() {
    let ctx = compose_reply_context(
        Some("[Replying to Adi: \"toolkuy-avatar-1080.png\"]".to_string()),
        "Adi",
        Some(media_reference_note("photo", Some("/tmp/photo_91.jpg"))),
    )
    .expect("a context");
    assert!(ctx.contains("toolkuy-avatar-1080.png"), "{ctx}");
    assert!(ctx.contains("<<IMG:/tmp/photo_91.jpg>>"), "{ctx}");
}

#[test]
fn a_captionless_media_reply_still_produces_context() {
    // The caption-only path returned None here, so the agent was not even told
    // a reply had happened.
    let ctx = compose_reply_context(None, "Adi", Some(media_reference_note("photo", None)))
        .expect("media alone must produce a context");
    assert!(ctx.contains("Replying to Adi"), "{ctx}");
    assert!(ctx.contains("could not be resolved"), "{ctx}");
}

#[test]
fn a_text_reply_is_unchanged_by_the_media_leg() {
    let plain = Some("[Replying to Adi: \"halo\"".to_string() + "]");
    assert_eq!(
        compose_reply_context(plain.clone(), "Adi", None),
        plain,
        "no media note means the context passes through untouched"
    );
    assert_eq!(compose_reply_context(None, "Adi", None), None);
}

#[test]
fn the_chat_id_in_this_fixture_matches_the_field_report() {
    // Guards the fixture itself: the report's chat is the one reproduced here.
    assert_eq!(photo_message("x").chat.id, ChatId(-1004367145331));
    assert_eq!(photo_message("x").id, MessageId(91));
}

#[test]
fn the_stored_tag_is_stripped_from_the_quoted_caption() {
    let row = "Ini wajah gua bro\n[file: /tmp/photo_91.jpg]";
    assert_eq!(
        strip_attachment_tag(row),
        "Ini wajah gua bro",
        "the note already names the file; the tag inside the quote is noise"
    );
    assert_eq!(strip_attachment_tag("[file: /tmp/photo_91.jpg]"), "");
    assert_eq!(
        strip_attachment_tag("halo bro"),
        "halo bro",
        "content without a tag passes through untouched"
    );
}
