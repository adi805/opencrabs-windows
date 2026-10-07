//! Outbound photo album tests (#97).
//!
//! `send_photo` used to take a single `photo_url`, so a set of N images left
//! as N separate notifications. `photo_urls` now delivers them as one album
//! via `send_media_group`.
//!
//! The split rules are the interesting part: the Bot API refuses an album of
//! fewer than 2 or more than 10 items, so the chunking has to be decided
//! before the request is built, and no photo may be dropped. These tests pin
//! `album_plan` and the `photo_urls` parser, which are the two pure pieces of
//! that path. The request itself needs a live bot, so it is not exercised
//! here; the parser tests are what keep a bad input from reaching it.

use crate::brain::tools::telegram_send::{TelegramSendTool, photo_refs};
use crate::brain::tools::r#trait::Tool;
use crate::channels::telegram::send::{MAX_ALBUM_SIZE, album_plan};
use serde_json::json;
use std::sync::Arc;

// ── album_plan: the split ──────────────────────────────────────────────

#[test]
fn single_photo_is_not_an_album() {
    // A 1-item album is refused by the Bot API, so the caller sends it with
    // send_photo. The plan still describes it as one group of 1.
    assert_eq!(album_plan(1), vec![1]);
}

#[test]
fn two_photos_are_one_album() {
    assert_eq!(album_plan(2), vec![2]);
}

#[test]
fn full_album_is_a_single_group() {
    assert_eq!(album_plan(MAX_ALBUM_SIZE), vec![MAX_ALBUM_SIZE]);
}

#[test]
fn eleven_photos_pad_the_remainder_instead_of_leaving_one() {
    // Naive chunking gives [10, 1] and the second request is refused.
    assert_eq!(album_plan(11), vec![9, 2]);
}

#[test]
fn twelve_photos_are_ten_plus_two() {
    assert_eq!(album_plan(12), vec![10, 2]);
}

#[test]
fn twenty_one_photos_are_ten_nine_two() {
    assert_eq!(album_plan(21), vec![10, 9, 2]);
}

#[test]
fn twenty_two_photos_are_ten_ten_two() {
    assert_eq!(album_plan(22), vec![10, 10, 2]);
}

#[test]
fn no_photos_is_an_empty_plan() {
    assert!(album_plan(0).is_empty());
}

#[test]
fn every_plan_sums_to_the_input_and_never_holds_a_lone_photo() {
    for count in 1..=200usize {
        let plan = album_plan(count);
        let sum: usize = plan.iter().sum();
        assert_eq!(sum, count, "plan for {count} dropped photos: {plan:?}");
        for size in &plan {
            assert!(
                *size <= MAX_ALBUM_SIZE,
                "album too big for {count}: {plan:?}"
            );
            if count > 1 {
                assert!(*size >= 2, "lone-item album for {count}: {plan:?}");
            }
        }
    }
}

// ── photo_refs: what the caller passed ─────────────────────────────────

#[test]
fn photo_urls_array_is_read_in_order() {
    let input = json!({"photo_urls": ["/tmp/a.png", "/tmp/b.png", "/tmp/c.png"]});
    let refs = photo_refs(&input).expect("array should parse");
    assert_eq!(refs, vec!["/tmp/a.png", "/tmp/b.png", "/tmp/c.png"]);
}

#[test]
fn photo_urls_takes_precedence_over_photo_url() {
    let input = json!({
        "photo_urls": ["/tmp/a.png", "/tmp/b.png"],
        "photo_url": "/tmp/ignored.png"
    });
    let refs = photo_refs(&input).expect("array should parse");
    assert_eq!(refs.len(), 2);
    assert!(!refs.contains(&"/tmp/ignored.png".to_string()));
}

#[test]
fn a_single_photo_url_still_works() {
    let input = json!({"photo_url": "https://example.com/cat.png"});
    let refs = photo_refs(&input).expect("single url should parse");
    assert_eq!(refs, vec!["https://example.com/cat.png"]);
}

#[test]
fn an_array_of_one_stays_one_photo() {
    let input = json!({"photo_urls": ["/tmp/only.png"]});
    let refs = photo_refs(&input).expect("array should parse");
    assert_eq!(refs.len(), 1);
}

#[test]
fn an_empty_array_is_an_error_not_an_empty_album() {
    let input = json!({"photo_urls": []});
    let err = photo_refs(&input).expect_err("empty array must be rejected");
    assert!(
        err.error.unwrap_or_default().contains("empty"),
        "error should say the list was empty"
    );
}

#[test]
fn a_non_string_entry_is_rejected_with_its_index() {
    let input = json!({"photo_urls": ["/tmp/a.png", 7]});
    let err = photo_refs(&input).expect_err("non-string entry must be rejected");
    let msg = err.error.unwrap_or_default();
    assert!(
        msg.contains("photo_urls[1]"),
        "error should name the bad index, got: {msg}"
    );
}

#[test]
fn an_empty_string_entry_is_rejected() {
    let input = json!({"photo_urls": ["/tmp/a.png", ""]});
    assert!(photo_refs(&input).is_err());
}

#[test]
fn a_non_array_photo_urls_is_rejected() {
    let input = json!({"photo_urls": "/tmp/a.png"});
    assert!(photo_refs(&input).is_err());
}

#[test]
fn missing_both_parameters_is_rejected() {
    let input = json!({});
    assert!(photo_refs(&input).is_err());
}

// ── schema contract ────────────────────────────────────────────────────

#[test]
fn schema_advertises_the_album_parameter() {
    let tool = TelegramSendTool::new(Arc::default());
    let schema = tool.input_schema();
    let desc = schema
        .pointer("/properties/photo_urls/description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        desc.contains("album"),
        "photo_urls must be documented as an album, got: {desc}"
    );
    assert_eq!(
        schema
            .pointer("/properties/photo_urls/type")
            .and_then(|v| v.as_str()),
        Some("array"),
        "photo_urls must be an array"
    );
}
