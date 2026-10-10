//! Dispatcher-level mouse-report fragment filter (#1983).
//!
//! When the escape read splits mid-burst, crossterm re-publishes SGR (1006)
//! and URXVT (1015) mouse reports as individual `Key(Char)` events. The
//! #1943 gate only guarded the chat plain-char path (`input.rs`), so every
//! other `Char` consumer - onboarding fields, modal dialogs, mission
//! control, the sudo prompt, session rename - happily typed the burst into
//! its buffer, and burst digits could even select onboarding wizard
//! options. This filter sits at the single dispatch choke point
//! (`App::handle_key_event`) so no mode has to remember to gate.
//!
//! The suppression decision itself is `is_mouse_sequence_fragment` - no new
//! detection logic, same semantics the chat path already shipped with,
//! including the accepted trade-off that a literal `[` swallows the burst
//! lead bytes that follow it (a typed `arr[3;4]` loses the digits today
//! exactly as it did before this module existed).

use crossterm::event::KeyCode;

use super::input::is_mouse_sequence_fragment;

/// Tail window kept for detection - matches the look-back inside
/// `is_mouse_sequence_fragment` (30 bytes).
const TAIL_WINDOW: usize = 30;

#[derive(Debug, Default)]
pub(crate) struct MouseFragGate {
    /// Mirror of the destination buffer's tail: chars that passed the gate
    /// are appended (they land in whatever buffer the mode writes to);
    /// suppressed burst chars are not, exactly like the chat path.
    tail: String,
    /// A real `KeyCode::Esc` event just passed. When the burst's `\x1b` was
    /// consumed as the Esc key, the following `[` is the burst head - drop
    /// it and seed the tail so the existing branches suppress the rest.
    esc_key_pending: bool,
}

impl MouseFragGate {
    /// Decide one key code: `true` means the event is a mouse-report
    /// fragment and must not reach any mode handler.
    pub(crate) fn absorb(&mut self, code: &KeyCode) -> bool {
        match code {
            KeyCode::Esc => {
                self.esc_key_pending = true;
                false
            }
            KeyCode::Char(c) => {
                let dropped = if self.esc_key_pending {
                    self.esc_key_pending = false;
                    if *c == '[' {
                        self.tail.push('[');
                        true
                    } else {
                        is_mouse_sequence_fragment(*c, &self.tail, self.tail.len())
                    }
                } else {
                    is_mouse_sequence_fragment(*c, &self.tail, self.tail.len())
                };
                if !dropped {
                    self.tail.push(*c);
                    if self.tail.len() > TAIL_WINDOW {
                        let mut cut = self.tail.len() - TAIL_WINDOW;
                        while !self.tail.is_char_boundary(cut) {
                            cut += 1;
                        }
                        self.tail.drain(..cut);
                    }
                }
                dropped
            }
            _ => {
                // Any other key (Enter, arrows, F12, …) neither forms nor
                // continues a burst; reset so stale tails can't suppress
                // later typing.
                self.esc_key_pending = false;
                self.tail.clear();
                false
            }
        }
    }
}
