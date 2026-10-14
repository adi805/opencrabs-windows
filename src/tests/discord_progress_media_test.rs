//! A file a tool streams through a progress event must reach the channel.
//!
//! `whatsapp_connect` delivers the pairing QR as a
//! `ProgressEvent::IntermediateText` carrying an `<<IMG:path>>` marker. The
//! Discord progress path bound that marker's paths to `_` and threw them away,
//! while the final-response path kept and shipped them. A tool that delivers a
//! file mid-turn therefore had it silently dropped: only the unicode fallback
//! survived, and pasted into a non-monospace message it is unscannable, so
//! pairing from Discord was impossible (#248).
//!
//! These tests read the handler source and pin the two properties the fix
//! rests on: the progress arm keeps the extracted paths, and it ships them
//! before the empty-body early return that would otherwise swallow them.

const HANDLER: &str = "src/channels/discord/handler.rs";

fn handler() -> String {
    let path = format!("{}/{HANDLER}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

#[test]
fn progress_path_keeps_media_markers() {
    let src = handler();
    assert!(
        !src.contains("let (clean, _) = crate::utils::extract_img_markers(&clean);"),
        "the progress path discards <<IMG:>> markers again (#248): a file \
         delivered through a progress event never reaches the channel"
    );
    assert!(
        !src.contains("let (clean, _) = crate::utils::extract_vid_markers(&clean);"),
        "the progress path discards <<VID:>> markers again (#248)"
    );
    assert!(
        src.contains("let (clean, img_paths) = crate::utils::extract_img_markers(&clean);"),
        "the progress path must keep the extracted image paths (#248)"
    );
}

#[test]
fn progress_media_is_sent_before_the_empty_body_return() {
    let src = handler();
    let keep = src
        .find("let (clean, img_paths) = crate::utils::extract_img_markers(&clean);")
        .expect("image marker extraction missing (#248)");
    let send = keep
        + src[keep..]
            .find("writes::send(&http, channel, message, Class::Create)")
            .expect("progress media is extracted but never sent (#248)");
    let early_return = send
        + src[send..]
            .find("if clean.trim().is_empty()")
            .expect("empty-body guard no longer follows the media send; update this test");
    assert!(
        send < early_return,
        "progress media must be sent before the empty-body return (#248): a \
         body that strips to nothing would otherwise swallow the attachment"
    );
}
