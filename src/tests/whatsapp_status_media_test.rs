//! Tests for the image path of `status_update` (#1485 follow-up).
//!
//! The status arm used to be text-only: `status()` in the pinned lib can post
//! an image (`send_image`), but nothing in OpenCrabs reached it. These tests pin
//! the two halves that can be checked without a live WhatsApp client — the
//! thumbnail builder's behavior, and the wiring in the dispatch arm (via a
//! source scan, the same technique the send/reply accounting tests use).

use crate::brain::tools::whatsapp_send::{STATUS_THUMBNAIL_MAX_EDGE, make_jpeg_thumbnail};

/// Encode an RGB gradient as PNG bytes, so the decoder has real work to do.
fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 251) as u8, (y % 241) as u8, 128])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode test PNG");
    out.into_inner()
}

#[test]
fn thumbnail_is_jpeg_within_the_max_edge() {
    let png = png_bytes(640, 480);
    let thumb = make_jpeg_thumbnail(&png).expect("a PNG must produce a thumbnail");

    assert_eq!(thumb[0], 0xFF, "JPEG SOI marker, byte 0");
    assert_eq!(thumb[1], 0xD8, "JPEG SOI marker, byte 1");

    let decoded = image::load_from_memory(&thumb).expect("the thumbnail must decode");
    assert!(
        decoded.width() <= STATUS_THUMBNAIL_MAX_EDGE
            && decoded.height() <= STATUS_THUMBNAIL_MAX_EDGE,
        "thumbnail {}x{} exceeds the {}px cap",
        decoded.width(),
        decoded.height(),
        STATUS_THUMBNAIL_MAX_EDGE
    );
    // A 32px JPEG is a few hundred bytes. If it ever balloons, the thumbnail is
    // carrying the full image and the cap stopped working.
    assert!(thumb.len() < 4096, "thumbnail is {} bytes", thumb.len());
}

#[test]
fn thumbnail_rejects_non_image_bytes() {
    assert!(make_jpeg_thumbnail(b"this is not an image").is_none());
    assert!(make_jpeg_thumbnail(&[]).is_none());
    // A PDF header is a plausible mistake for `media_path` and must not pass.
    assert!(make_jpeg_thumbnail(b"%PDF-1.7\n").is_none());
}

// ── Source-scan sentinels: the dispatch wiring ───────────────────────

fn status_arm() -> &'static str {
    const SRC: &str = include_str!("../brain/tools/whatsapp_send.rs");
    let (_, rest) = SRC
        .split_once("\"status_update\" =>")
        .expect("status_update arm not found");
    let (arm, _) = rest
        .split_once("\"list_newsletters\" =>")
        .expect("list_newsletters anchor not found");
    arm
}

#[test]
fn status_arm_posts_an_image_through_the_lib() {
    let arm = status_arm();
    assert!(
        arm.contains("make_jpeg_thumbnail(&bytes)"),
        "status arm does not build the JPEG thumbnail an image status requires"
    );
    assert!(
        arm.contains("wacore::download::MediaType::Image"),
        "status arm does not upload the image"
    );
    assert!(
        arm.contains("send_image(upload, thumbnail, text.as_deref(), &jids, opts)"),
        "status arm does not call the lib's send_image"
    );
}

#[test]
fn status_arm_keeps_the_text_path() {
    let arm = status_arm();
    assert!(
        arm.contains(".send_text("),
        "the text status path disappeared; status_update must still do both"
    );
    assert!(
        arm.contains("STATUS_BACKGROUND_ARGB"),
        "text status lost its background colour"
    );
}

#[test]
fn status_arm_refuses_a_non_image_media_path() {
    let arm = status_arm();
    assert!(
        arm.contains("mime.starts_with(\"image/\")"),
        "a document or audio path would be uploaded as an image status"
    );
    assert!(
        arm.contains("could not be decoded as an image"),
        "undecodable media is not refused"
    );
}

#[test]
fn status_arm_refuses_an_empty_status() {
    let arm = status_arm();
    assert!(
        arm.contains("text.is_none() && media_path.is_none()"),
        "a status with neither text nor media would be posted empty"
    );
}

#[test]
fn schema_advertises_status_media_path() {
    const SRC: &str = include_str!("../brain/tools/whatsapp_send.rs");
    assert!(
        SRC.contains("For status_update: a local image file to post as an image status"),
        "media_path's description does not mention status_update, so an agent cannot discover the image path"
    );
}
