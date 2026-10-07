//! Tests for `MouseFragGate`, the dispatcher-level mouse-report fragment
//! filter (#1983). The #1943 gate only ran in the chat plain-char branch,
//! so SGR/URXVT bursts split into individual `Key(Char)` events still
//! flooded onboarding fields, modal dialogs, mission control, the sudo
//! prompt and the session-rename buffer - and burst digits could silently
//! select wizard options. The gate now sits at the single key-dispatch
//! choke point; these tests pin its behavior.
//!
//! Sequences below model what crossterm delivers when the leading `\x1b`
//! was already consumed: the visible burst chars arrive one KeyEvent at a
//! time, in order.

use crate::tui::app::mouse_frag::MouseFragGate;
use crossterm::event::KeyCode;

fn chars(s: &str) -> Vec<KeyCode> {
    s.chars().map(KeyCode::Char).collect()
}

/// Feed `codes` through a fresh gate; return the ones that were DROPPED.
fn dropped(codes: &[KeyCode]) -> Vec<KeyCode> {
    let mut gate = MouseFragGate::default();
    codes.iter().filter(|c| gate.absorb(c)).cloned().collect()
}

#[test]
fn sgr_burst_body_is_dropped_from_cold_start() {
    // "[<35;98;25M" - the exact flood shape from the 2026-10-07 screenshot.
    let burst = chars("[<35;98;25M");
    let gone = dropped(&burst);
    // Leading `[` passes (parity with the shipped #1943 gate, which lets a
    // bare bracket through as ordinary text); everything after it is eaten.
    assert_eq!(gone, chars("<35;98;25M"));
}

#[test]
fn urxvt_burst_body_is_dropped() {
    // "[35;98;25M" (mode 1015 shape): '[' passes, digits/;/M dropped.
    let burst = chars("[35;98;25M");
    assert_eq!(dropped(&burst), chars("35;98;25M"));
}

#[test]
fn raw_esc_char_is_dropped_immediately() {
    let mut gate = MouseFragGate::default();
    assert!(gate.absorb(&KeyCode::Char('\x1b')));
    assert!(gate.absorb(&KeyCode::Char('\u{3}')));
}

#[test]
fn esc_key_arms_the_split_burst_and_drops_the_bracket() {
    // When the burst's ESC arrived as KeyCode::Esc, the app still sees the
    // cancel (Esc passes), but the following '[' is the burst head - unlike
    // the chat gate, this one leaves no stray bracket behind.
    let mut gate = MouseFragGate::default();
    assert!(!gate.absorb(&KeyCode::Esc), "Esc must stay a cancel key");
    assert!(gate.absorb(&KeyCode::Char('[')), "burst '[' must be eaten");
    for c in chars("<35;98;25M") {
        assert!(gate.absorb(&c), "{c:?} is burst tail");
    }
    // State must be fully released for ordinary typing after the burst.
    for c in chars("ok") {
        assert!(!gate.absorb(&c));
    }
}

#[test]
fn burst_digits_never_reach_a_handler() {
    // The wizard-selection hazard: digits at onboarding/input.rs:985 pick
    // options, so a burst must deliver ZERO digits to any handler.
    let mut gate = MouseFragGate::default();
    gate.absorb(&KeyCode::Esc);
    let leaked: Vec<KeyCode> = chars("[<35;98;25M[<35;98;25M")
        .into_iter()
        .filter(|c| !gate.absorb(c))
        .collect();
    assert!(
        leaked
            .iter()
            .all(|c| !matches!(c, KeyCode::Char(ch) if ch.is_ascii_digit())),
        "burst digits leaked: {leaked:?}"
    );
}

#[test]
fn repeated_bursts_do_not_accumulate_garbage() {
    // Twenty consecutive motion reports (the screenshot showed dozens).
    let mut gate = MouseFragGate::default();
    let mut leaked = String::new();
    for _ in 0..20 {
        for c in chars("\x1b[<35;98;25M") {
            if !gate.absorb(&c) {
                leaked.push(match c {
                    KeyCode::Char(ch) => ch,
                    _ => '?',
                });
            }
        }
    }
    // ESC chars all dropped; the only residue is the leading bracket each
    // burst's '[' survives as - bounded, scrubbed at submit by
    // strip_terminal_escapes. No digit/;/M flood can ever form.
    assert_eq!(leaked.chars().filter(|c| !matches!(c, '[')).count(), 0);
}

#[test]
fn ordinary_typing_flows_and_resets_between_bursts() {
    let mut gate = MouseFragGate::default();
    for c in chars("ping -c1 example.com") {
        assert!(!gate.absorb(&c), "ordinary text {c:?} must pass");
    }
    // Enter clears the tail mirror (word boundaries can't chain into a burst).
    assert!(!gate.absorb(&KeyCode::Enter));
    for c in chars("[3;4]") {
        gate.absorb(&c);
    }
    // After a non-Char key, state is clean again.
    assert!(!gate.absorb(&KeyCode::Left));
    for c in chars("7") {
        assert!(!gate.absorb(&c), "digit after reset must pass");
    }
}

#[test]
fn long_prefix_still_detects_burst_within_window() {
    // Typing well past the 32-byte look-back window must not weaken
    // detection: the burst's own '[' arms the tail at the window edge.
    let mut gate = MouseFragGate::default();
    for c in chars(&"x".repeat(60)) {
        assert!(!gate.absorb(&c));
    }
    for c in chars("[<35;98;25M") {
        gate.absorb(&c);
    }
    // Tail released once typing a normal word.
    assert!(!gate.absorb(&KeyCode::Enter));
    for c in chars("done") {
        assert!(!gate.absorb(&c));
    }
}

// ── dispatch-level feed: split bursts injected ahead of a real mode ──
//
// The dispatcher gate (#1983) runs before ANY mode handler, so the same
// suppression that protects the chat buffer must protect the onboarding
// wizard's text fields too - burst digits used to silently pick wizard
// options. These tests model the dispatcher exactly: every key goes
// through `MouseFragGate::absorb` first; only keys that pass reach the
// mode's own `handle_key`.

use crate::tui::onboarding::{BrainField, OnboardingStep, OnboardingWizard};
use crossterm::event::{KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn brain_wizard() -> OnboardingWizard {
    let mut w = OnboardingWizard::new();
    w.step = OnboardingStep::BrainSetup;
    w.brain_field = BrainField::AboutMe;
    // `new()` seeds `about_me` from any existing USER.md (the wizard opens
    // on the Review screen with the profile prefilled). Clear it so the
    // burst-feed assertions measure exactly what the feed typed.
    w.about_me.clear();
    w
}

#[test]
fn dispatch_level_feed_keeps_onboarding_field_free_of_burst_bytes() {
    // Cold gate, three SGR bursts (the '[' lead survives per the #1943
    // trade-off, documented in mouse_frag.rs; nothing else may).
    let mut gate = MouseFragGate::default();
    let mut w = brain_wizard();
    for _ in 0..3 {
        for c in chars("[<35;98;25M") {
            if gate.absorb(&c) {
                continue;
            }
            w.handle_key(key(c));
        }
    }
    assert!(
        !w.about_me
            .chars()
            .any(|ch| ch.is_ascii_digit() || ch == ';' || ch == 'M' || ch == '<'),
        "burst bytes leaked into the wizard field: {:?}",
        w.about_me
    );
    assert!(
        matches!(w.step, OnboardingStep::BrainSetup),
        "burst must never navigate the wizard"
    );
}

#[test]
fn dispatch_level_feed_after_esc_key_delivers_nothing_to_the_field() {
    // The #1983 flood shape: crossterm surfaced every burst's ESC as
    // KeyCode::Esc (the dispatcher passes Esc straight to the mode as its
    // cancel key - the gate's own Esc-arming below models the char stream
    // only, so the wizard isn't cancelled mid-test), then the report body
    // arrives as individual chars, one Esc per burst. Each Esc re-arms the
    // gate, so it eats each body byte-for-byte INCLUDING the leading '[';
    // zero survivors reach the field. A cold (never-armed) tail may leak a
    // lone '[' per burst, the documented #1943 trade-off the test above
    // pins.
    let mut gate = MouseFragGate::default();
    let mut w = brain_wizard();
    for _ in 0..3 {
        assert!(
            !gate.absorb(&KeyCode::Esc),
            "Esc must pass as the cancel key"
        );
        for c in chars("[<35;98;25M") {
            if gate.absorb(&c) {
                continue;
            }
            w.handle_key(key(c));
        }
    }
    assert!(
        w.about_me.is_empty(),
        "not a single burst byte may reach the field: {:?}",
        w.about_me
    );
    assert!(
        matches!(w.step, OnboardingStep::BrainSetup),
        "burst must never navigate the wizard"
    );
}
