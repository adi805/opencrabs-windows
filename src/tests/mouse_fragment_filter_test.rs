//! Tests for `is_mouse_sequence_fragment`, the filter that drops mouse
//! tracking CSI bytes that crossterm hands us as individual `Key(Char)`
//! events when the leading `\x1b` was already consumed as `KeyCode::Esc`.
//!
//! Two formats leak this way (both enabled by `EnableMouseCapture`):
//!   - SGR (1006): `\x1b[<Cb;Cx;Cy[Mm]` → visible chars `[<…M/m`
//!   - URXVT (1015): `\x1b[Cb;Cx;Cy M`  → visible chars `[N;…M`
//!
//! The 2026-04 leak that drove this regression test was URXVT-format
//! motion bursts (e.g. `[35;1;101M`) flooding the input buffer when a
//! `bash` tool was running — the screenshot showed dozens of these
//! reports concatenated and lagging the keyboard.

use crate::tui::app::input::is_mouse_sequence_fragment;

// --- SGR mode (1006) — already supported before this fix ---

#[test]
fn sgr_suppresses_digit_after_bracket_lt() {
    // After `[<` we know we're inside an SGR mouse sequence.
    assert!(is_mouse_sequence_fragment('3', "[<", 2));
}

#[test]
#[allow(non_snake_case)]
fn sgr_suppresses_digits_through_terminator_M() {
    let buf = "[<35;116;77";
    assert!(is_mouse_sequence_fragment('M', buf, buf.len()));
}

#[test]
fn sgr_suppresses_lowercase_m_release() {
    let buf = "[<35;116;77";
    assert!(is_mouse_sequence_fragment('m', buf, buf.len()));
}

// --- URXVT mode (1015) — added by this commit ---

#[test]
fn urxvt_suppresses_digit_when_bracket_followed_by_digits() {
    // `[35` already in buffer (URXVT cb partially typed). Next digit is
    // part of the same sequence.
    let buf = "[35";
    assert!(is_mouse_sequence_fragment('1', buf, buf.len()));
}

#[test]
fn urxvt_suppresses_semicolon_separator() {
    let buf = "[35";
    assert!(is_mouse_sequence_fragment(';', buf, buf.len()));
}

#[test]
#[allow(non_snake_case)]
fn urxvt_suppresses_terminator_M() {
    // Full URXVT sequence `[35;1;101` waiting for terminator.
    let buf = "[35;1;101";
    assert!(is_mouse_sequence_fragment('M', buf, buf.len()));
}

#[test]
fn urxvt_suppresses_digit_after_first_bracket() {
    // The defensive rule: any digit immediately after `[` is treated as
    // the start of a URXVT cb, since legit `[N…]` typing is rare and
    // mouse motion bursts are not.
    let buf = "[";
    assert!(is_mouse_sequence_fragment('3', buf, buf.len()));
}

#[test]
#[allow(non_snake_case)]
fn urxvt_suppresses_full_sequence_through_to_M() {
    // Walk the whole sequence char by char and verify each one would
    // have been suppressed (the loud-failure case from the screenshot).
    let chars = ['[', '3', '5', ';', '1', ';', '1', '0', '1', 'M'];
    let mut buf = String::new();
    let mut suppressed_count = 0;
    for c in chars {
        if is_mouse_sequence_fragment(c, &buf, buf.len()) {
            suppressed_count += 1;
        } else {
            buf.push(c);
        }
    }
    // The first `[` slips through (no prior context to flag it); every
    // other char must be suppressed.
    assert_eq!(buf, "[", "only the bare `[` should remain in the buffer");
    assert_eq!(suppressed_count, chars.len() - 1);
}

#[test]
fn urxvt_suppresses_back_to_back_sequences_minus_lone_brackets() {
    // Two URXVT motion reports back-to-back — what mouse drag produces.
    // Same expectation: only the `[` of each report leaks.
    let chars = "[35;1;101M[35;1;102M".chars().collect::<Vec<_>>();
    let mut buf = String::new();
    for c in chars {
        if !is_mouse_sequence_fragment(c, &buf, buf.len()) {
            buf.push(c);
        }
    }
    assert_eq!(buf, "[[", "only the bracket from each report should leak");
}

// --- Real-text false-positive guards ---

#[test]
fn does_not_fire_on_normal_text() {
    // Regular typing should never trigger the filter.
    let buf = "hello world";
    assert!(!is_mouse_sequence_fragment('!', buf, buf.len()));
    assert!(!is_mouse_sequence_fragment(' ', buf, buf.len()));
    assert!(!is_mouse_sequence_fragment('a', buf, buf.len()));
}

#[test]
fn does_not_fire_on_chars_outside_mouse_alphabet() {
    // The fast-path early return — if the char isn't in the mouse
    // alphabet (digits / `;` / `[<>Mm`) it can never be part of one.
    let buf = "[35;1;101";
    assert!(!is_mouse_sequence_fragment('a', buf, buf.len()));
    assert!(!is_mouse_sequence_fragment(' ', buf, buf.len()));
    assert!(!is_mouse_sequence_fragment('.', buf, buf.len()));
}

#[test]
fn fires_on_semicolon_after_bracket_letter_combo_breaks_pattern() {
    // After `[abc` (letters break the URXVT cb pattern), a `;` should
    // not fire — the sequence isn't valid URXVT.
    let buf = "[abc";
    assert!(!is_mouse_sequence_fragment(';', buf, buf.len()));
}

#[test]
fn handles_multibyte_chars_safely() {
    // The 30-byte tail window can land inside a multi-byte char; the
    // function snaps to a char boundary instead of panicking.
    let mut buf = "🦀".repeat(8);
    buf.push_str("[35");
    let cursor = buf.len();
    // Should not panic.
    let _ = is_mouse_sequence_fragment('1', &buf, cursor);
}

// --- Raw ESC / ETX arriving as chars (#1943) ---

#[test]
fn raw_esc_char_is_fragment() {
    // Crossterm reports Esc as KeyCode::Esc, so a bare ESC reaching the
    // char path means a split escape read, never legitimate typing.
    // The gate fires regardless of buffer tail.
    assert!(is_mouse_sequence_fragment('\x1b', "File both issues", 14));
    assert!(is_mouse_sequence_fragment('\x1b', "", 0));
}

#[test]
fn raw_etx_char_is_fragment() {
    // The `^C` visible in the #1943 screenshot: Ctrl+C leaked through as
    // a raw 0x03 char. Not text the user asked for.
    assert!(is_mouse_sequence_fragment(
        '\u{3}',
        "[<35;10;102Mgarbage",
        13
    ));
    assert!(is_mouse_sequence_fragment('\u{3}', "plain", 5));
}
