//! #1964 regression: the terminal restore sequence must disable EVERY mode
//! the TUI turns on. The Oct 2026 incident: the crash-recovery path in
//! src/cli/ui.rs emitted only raw-mode-off + alt-screen-leave, so mouse
//! capture survived the death and zsh echoed every mouse wiggle as
//! `^[[<35;…M` garbage until the next restart. The byte-level pin here is
//! what that path stopped paying for.

/// The full restore sequence contains every DECSET-off a live TUI switched
/// on: SGR mouse + any-event + normal mouse, focus tracking, bracketed
/// paste, alternate screen, and cursor show (verified against the
/// crossterm 0.29 emitter sources for each command).
#[test]
fn restore_sequence_turns_off_every_tui_terminal_mode() {
    let mut buf: Vec<u8> = Vec::new();
    crate::tui::runner::restore_to(&mut buf).expect("restore to a Vec must not fail");
    let s = String::from_utf8_lossy(&buf);

    for code in [
        "?1000l", // normal mouse tracking off
        "?1003l", // any-event mouse tracking off
        "?1006l", // SGR mouse coordinates off (the ^[[<35;x;yM source)
        "?1004l", // focus tracking off
        "?2004l", // bracketed paste off
        "?1049l", // alternate screen left
        "?25h",   // cursor shown again
    ] {
        assert!(
            s.contains(code),
            "restore sequence missing {code}; got: {s:?}"
        );
    }
}
