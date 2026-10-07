//! #1897: RTL text must reach the screen shaped and in visual order.
//!
//! The TUI emitted logical-order Arabic/Hebrew into a left-to-right cell
//! grid, so the terminal showed mirrored, disconnected glyphs. `apply_rtl`
//! runs the Arabic joining stage on logical order (context windows need
//! adjacency) and the Unicode Bidirectional Algorithm last, on the shaped
//! text (presentation forms keep their `AL` bidi class).

use crate::tui::render::utils::{apply_rtl, apply_rtl_lines, is_arabic_char, is_rtl_char};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

#[test]
fn latin_only_text_is_untouched() {
    // Fast path: None means zero allocation and zero change.
    assert_eq!(apply_rtl("hello world, cargo build --release"), None);
}

#[test]
fn hebrew_pure_run_is_reordered_not_shaped() {
    let input = "שלום";
    // Hebrew needs no joining stage, so the visual line is the plain
    // logical-order reversal.
    let expected: String = input.chars().rev().collect();
    assert_eq!(apply_rtl(input), Some(expected));
}

#[test]
fn arabic_is_shaped_then_reordered() {
    let input = "ادخل";
    let shaped = arabic_reshaper::arabic_reshape(input);
    let expected: String = shaped.chars().rev().collect();
    let out = apply_rtl(input).expect("Arabic input must produce visual order");
    assert_eq!(out, expected);
    // Regression: the bug emitted the logical string verbatim.
    assert_ne!(out, input);
}

#[test]
fn mixed_latin_line_keeps_latin_words_intact() {
    let input = "run cargo build في الطريق";
    let out = apply_rtl(input).expect("line contains RTL");
    // Latin run sits at the start of an LTR-base paragraph, so it must stay
    // contiguous and in order; only the embedded Arabic flips.
    assert!(out.contains("cargo build"));
    assert_ne!(out, input);
}

#[test]
fn span_pass_reorders_text_and_preserves_styles() {
    let line = Line::from(vec![
        Span::styled("שלום", Style::default().bold()),
        Span::raw("!!"),
    ]);
    let out = apply_rtl_lines(vec![line]);
    assert_eq!(out[0].spans.len(), 2);
    assert_eq!(out[0].spans[0].content.as_ref(), "םולש");
    assert!(out[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(out[0].spans[1].content.as_ref(), "!!");
}

#[test]
fn rtl_and_arabic_detection() {
    assert!(is_rtl_char('ش')); // Arabic SHIN, class AL
    assert!(is_rtl_char('ש')); // Hebrew ALEF, class R
    assert!(!is_rtl_char('a'));
    assert!(!is_rtl_char('5'));
    assert!(is_arabic_char('ش'));
    assert!(!is_arabic_char('ש'));
}
