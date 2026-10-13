use crate::channels::voice::service::*;

#[test]
fn split_short_text_returns_single_chunk() {
    let text = "Hello world, this is short.";
    let chunks = split_for_tts(text, 1500);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], text);
}

#[test]
fn split_long_text_breaks_at_sentences() {
    let text = "First sentence here. Second sentence here. Third sentence is much longer and goes on for a while to push us over the limit.";
    let chunks = split_for_tts(text, 50);
    assert!(chunks.len() > 1);
    // Each chunk should be under the limit
    for chunk in &chunks {
        assert!(chunk.len() <= 50, "chunk too long: {} chars", chunk.len());
    }
    // All content should be preserved
    let rejoined: String = chunks.join(" ");
    assert!(rejoined.contains("First sentence"));
    assert!(rejoined.contains("Second sentence"));
    assert!(rejoined.contains("Third sentence"));
}

#[test]
fn split_respects_question_and_exclamation() {
    let text = "Is this working? Yes it is! Great news indeed. Another sentence.";
    let chunks = split_for_tts(text, 30);
    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(chunk.len() <= 30, "chunk too long: {} chars", chunk.len());
    }
}

#[test]
fn split_falls_back_to_word_boundary() {
    // No sentence breaks, just one long sentence
    let text = "word ".repeat(100); // 500 chars, no periods
    let chunks = split_for_tts(&text, 100);
    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(chunk.len() <= 100, "chunk too long: {} chars", chunk.len());
    }
}

#[test]
fn find_sentence_break_finds_last_period() {
    assert_eq!(find_sentence_break("Hello. World"), Some(7)); // ". " is 2 chars
    assert_eq!(find_sentence_break("Hello! World"), Some(7));
    assert_eq!(find_sentence_break("Hello? World"), Some(7));
    assert_eq!(find_sentence_break("No break here"), None);
}

#[test]
fn split_preserves_newlines() {
    let text =
        "Paragraph one.\n\nParagraph two.\n\nParagraph three with more text to exceed limit.";
    let chunks = split_for_tts(text, 30);
    assert!(chunks.len() > 1);
}

#[test]
fn split_empty_text() {
    let chunks = split_for_tts("", 1500);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], "");
}

#[test]
fn split_exactly_at_limit() {
    let text = "a".repeat(1500);
    let chunks = split_for_tts(&text, 1500);
    assert_eq!(chunks.len(), 1);
}

#[test]
fn split_one_over_limit() {
    let text = "a".repeat(1501);
    let chunks = split_for_tts(&text, 1500);
    assert_eq!(chunks.len(), 2);
}

#[test]
fn split_realistic_long_response() {
    // Simulate a long assistant response by repeating meaningful text
    let base = "Here is a detailed explanation of how Rust's borrow checker works. The borrow checker is a compile-time feature that ensures memory safety. It tracks the lifetime of references and ensures they don't outlive the data they point to. ";
    let text = base.repeat(10); // ~2500 chars
    let chunks = split_for_tts(&text, 1500);
    assert!(
        chunks.len() >= 2,
        "expected >= 2 chunks, got {}",
        chunks.len()
    );
    for chunk in &chunks {
        assert!(chunk.len() <= 1500, "chunk too long: {} chars", chunk.len());
        assert!(!chunk.trim().is_empty(), "empty chunk");
    }
}

// ── COR-04: the byte-budget cut must land on a char boundary ──────

#[test]
fn split_does_not_panic_when_a_multibyte_char_straddles_the_limit() {
    // 1499 ASCII bytes, then a 4-byte emoji whose bytes straddle index 1500.
    // A raw `&remaining[..1500]` panics here: byte 1500 is inside the emoji.
    let mut text = "a".repeat(1499);
    text.push('🎧');
    text.push_str(&"b".repeat(50));
    assert_eq!(text.len(), 1553);

    let chunks = split_for_tts(&text, 1500);
    assert!(chunks.len() >= 2, "expected a split, got {}", chunks.len());
    // Nothing is lost and nothing is corrupted by the cut.
    assert_eq!(chunks.concat(), text);
    assert!(chunks.iter().all(|c| !c.is_empty()));
    assert_eq!(chunks.iter().filter(|c| c.contains('🎧')).count(), 1);
}

#[test]
fn split_handles_a_multibyte_whitespace_word_break() {
    // U+00A0 is a 2-byte whitespace, so `pos + 1` from the last whitespace
    // lands INSIDE it: the word-break path must use the char's length.
    let mut text = "a".repeat(90);
    text.push('\u{00A0}');
    text.push_str(&"b".repeat(90));

    let chunks = split_for_tts(&text, 100);
    assert!(chunks.len() >= 2, "expected a split, got {}", chunks.len());
    assert!(chunks.iter().all(|c| !c.trim().is_empty()));
    assert_eq!(chunks.concat().chars().filter(|c| *c == 'a').count(), 90);
    assert_eq!(chunks.concat().chars().filter(|c| *c == 'b').count(), 90);
}

#[test]
fn split_chunks_emoji_dense_text_without_loss() {
    // Every character is 4 bytes: the safe bound must still make progress.
    let text = "👋".repeat(200);
    let chunks = split_for_tts(&text, 300);
    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|c| !c.is_empty()));
    assert_eq!(chunks.concat(), text);
}

#[test]
fn split_makes_progress_when_the_budget_is_smaller_than_one_char() {
    // A budget of 1 byte cannot hold a 4-byte emoji: the splitter must take
    // the character whole rather than loop forever on a zero-width split.
    let text = "🎧🎧🎧";
    let chunks = split_for_tts(text, 1);
    assert_eq!(chunks.concat(), text);
    assert!(chunks.iter().all(|c| !c.is_empty()));
}
