//! Tests for `App::strip_terminal_escapes` and its ESC-less mouse-report
//! burst pass (#1943). When an escape read splits mid-burst, crossterm
//! never sees the leading `\x1b` and the remainder (`[<35;10;102M…`) lands
//! in the input buffer as plain text. The old fast path returned
//! ESC-free text verbatim, so a typed `/cmd` sat behind a wall of bursts
//! and slash dispatch (`starts_with('/')`) never matched for ~22 minutes.

use crate::tui::app::App;

#[test]
fn esc_less_sgr_burst_is_stripped() {
    let poisoned = "File both issues[<35;10;102M[<35;11;102M";
    assert_eq!(App::strip_terminal_escapes(poisoned), "File both issues");
}

#[test]
fn esc_less_urxvt_burst_is_stripped() {
    let poisoned = "hello[35;10;102M[35;11;102mworld";
    assert_eq!(App::strip_terminal_escapes(poisoned), "helloworld");
}

#[test]
fn burst_before_slash_command_is_stripped() {
    // The exact dispatch failure from the incident: strip restores the
    // leading `/` so `handle_slash_command` matches again.
    let poisoned = "[<35;10;102M/compact";
    assert_eq!(App::strip_terminal_escapes(poisoned), "/compact");
}

#[test]
fn full_csi_with_esc_still_stripped() {
    assert_eq!(App::strip_terminal_escapes("a\x1b[<35;10;102Mb"), "ab");
}

#[test]
fn etx_byte_is_dropped() {
    assert_eq!(App::strip_terminal_escapes("foo\x03bar"), "foobar");
}

#[test]
fn bracketed_bounds_text_survives() {
    // Ordinary bracketed text must never be eaten: no final M/m, too few
    // `;` segments, or a truncated tail all stay untouched.
    for s in [
        "arr[3;4]",
        "list[1;2",
        "note[<3;4]",
        "[<35;10;102",
        "a[35;10M b",
        "[<M]",
    ] {
        assert_eq!(App::strip_terminal_escapes(s), s, "{s:?} should survive");
    }
}

#[test]
fn multibyte_text_survives_burst_strip() {
    let s = "héllo 🦀[<35;10;102M";
    assert_eq!(App::strip_terminal_escapes(s), "héllo 🦀");
}

#[test]
fn clean_text_fast_path_returns_unchanged() {
    let s = "just typing, no brackets at all";
    assert_eq!(App::strip_terminal_escapes(s), s);
}

#[test]
fn osc_sequence_stripped_burst_kept() {
    // OSC (ESC ] … BEL) removal still works, and a surviving legit
    // bracket next to it is untouched.
    let s = "\x1b]0;title\x07keep[3;4]me";
    assert_eq!(App::strip_terminal_escapes(s), "keep[3;4]me");
}
