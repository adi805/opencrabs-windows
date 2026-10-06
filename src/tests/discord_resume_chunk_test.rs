//! The background resume producer posts a completed turn to Discord through
//! `resume_delivery_chunks` (#1899). A single `say` over 2000 characters is
//! rejected with `Message too large` and the whole verdict disappears, so the
//! seam must chunk exactly like the live delivery path does.

use crate::channels::discord::resume::resume_delivery_chunks;

fn prose_lines(count: usize) -> String {
    let mut text = String::new();
    for i in 0..count {
        text.push_str(&format!(
            "line {i:0>3}: the gate finished and reported the result for this row\n"
        ));
    }
    text
}

#[test]
fn resume_delivery_chunks_long_text_under_2000_each() {
    // A 5000+ char CI verdict: more than one message, none over Discord's limit.
    // Each prose line is 65 bytes, so 90 lines is 5850.
    let text = prose_lines(90);
    assert!(text.len() > 5000, "fixture too short: {} bytes", text.len());
    let chunks = resume_delivery_chunks(&text);
    assert!(
        chunks.len() > 1,
        "expected the verdict to split, got {} chunk(s)",
        chunks.len()
    );
    for chunk in &chunks {
        assert!(
            chunk.len() <= 2000,
            "chunk exceeds Discord's hard limit: {} bytes",
            chunk.len()
        );
    }
}

#[test]
fn resume_delivery_short_text_yields_single_chunk() {
    // The common case must not become two messages or gain any framing.
    let text = "All green: 41 tests passed, clippy clean.";
    assert_eq!(
        resume_delivery_chunks(text),
        vec![text.to_string()],
        "short verdict must travel as one unchanged message"
    );
}

#[test]
fn resume_delivery_preserves_all_content() {
    // Nothing may be dropped at a chunk boundary: rejoining the chunks rebuilds
    // the verdict byte for byte (plain prose, so no fence framing is inserted).
    let text = prose_lines(120);
    let chunks = resume_delivery_chunks(&text);
    assert!(chunks.len() > 1, "fixture must split to prove anything");
    let rejoined: String = chunks.concat();
    assert_eq!(rejoined.len(), text.len(), "content size changed");
    assert_eq!(rejoined, text, "chunks lost or reordered content");
}

#[test]
fn resume_delivery_splits_on_fence_without_open_markup() {
    // A fenced block longer than the limit (a table grid is usually fenced)
    // must not leave a chunk with an unclosed fence: Discord would render the
    // rest of the channel as code. Ties to #876.
    let mut text = String::from("verdict\n\n```text\n");
    for i in 0..120 {
        text.push_str(&format!("row-{i:0>3}          some columnar detail here\n"));
    }
    text.push_str("```\n\ndone\n");
    let chunks = resume_delivery_chunks(&text);
    assert!(chunks.len() > 1, "expected the fence to split");
    for chunk in &chunks {
        assert_eq!(
            chunk.matches('`').count() % 2,
            0,
            "unbalanced fence in {chunk:?}"
        );
        assert!(
            chunk.len() <= 2000,
            "chunk exceeds Discord's hard limit: {} bytes",
            chunk.len()
        );
    }
}

#[test]
fn resume_delivery_empty_content_sends_nothing() {
    // An empty `say` is a Discord 400, so blank output yields no chunks.
    assert!(
        resume_delivery_chunks("").is_empty(),
        "empty verdict must not be sent"
    );
    assert!(
        resume_delivery_chunks("   \n\t  ").is_empty(),
        "whitespace-only verdict must not be sent"
    );
}
