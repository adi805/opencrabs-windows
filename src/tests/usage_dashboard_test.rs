use crate::tui::app::dialog_keys::{DialogScope, dialog_keys};
use crate::usage::dashboard::*;
use crate::usage::data::Period;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

/// A bare key press, the shape the dashboard's keymap is written in.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn test_default_state() {
    let s = DashboardState::default();
    assert_eq!(s.period, Period::AllTime);
    assert_eq!(s.focused_card, 0);
}

#[test]
fn test_focus_cycling() {
    let mut s = DashboardState::default();
    assert_eq!(s.focused_card, 0);
    s.focus_next();
    assert_eq!(s.focused_card, 1);
    s.focus_next();
    assert_eq!(s.focused_card, 2);
    s.focus_next();
    assert_eq!(s.focused_card, 3);
    s.focus_next();
    assert_eq!(s.focused_card, 4);
    s.focus_next();
    assert_eq!(s.focused_card, 5);
    s.focus_next();
    assert_eq!(s.focused_card, 0); // wraps

    s.focus_prev();
    assert_eq!(s.focused_card, 5); // wraps back
    s.focus_prev();
    assert_eq!(s.focused_card, 4);
}

#[test]
fn test_set_period() {
    let mut s = DashboardState::default();
    assert!(s.set_period(Period::Today));
    assert!(!s.set_period(Period::Today)); // same, no change
    assert!(s.set_period(Period::Week));
}

#[test]
fn test_centered_rect_large_terminal() {
    let area = Rect::new(0, 0, 200, 60);
    let r = centered_rect(area);
    assert_eq!(r.width, 150); // 75% of 200
    assert_eq!(r.height, 45); // 75% of 60
    assert_eq!(r.x, 25); // centered
    assert_eq!(r.y, 7); // centered (rounding)
}

#[test]
fn test_centered_rect_small_terminal() {
    let area = Rect::new(0, 0, 80, 24);
    let r = centered_rect(area);
    assert_eq!(r.width, 60); // 75% of 80 = 60, meets floor of 60
    assert_eq!(r.height, 20); // max(75%=18, floor=20) = 20, capped at 22
}

#[test]
fn test_centered_rect_tiny_terminal() {
    let area = Rect::new(0, 0, 40, 15);
    let r = centered_rect(area);
    // Should not exceed area
    assert!(r.width <= area.width);
    assert!(r.height <= area.height);
    assert!(r.x >= area.x);
    assert!(r.y >= area.y);
}

// ── Key decision (#2027) ─────────────────────────────────────────────────────

/// DRIFT TEST — the footer's contract with the input handler.
///
/// With the breakdown closed, a key appears in the footer exactly when the
/// handler does something with it. That equivalence is what makes Enter
/// disappear off the Cache Efficiency card, where it used to be advertised and
/// silently dropped, and it keeps every other advertised key honest.
#[test]
fn footer_advertises_exactly_the_keys_the_handler_honours() {
    for card in 0..CARD_COUNT {
        let state = DashboardState {
            focused_card: card,
            ..Default::default()
        };
        let advertised = advertised_keys(&state);
        for entry in dialog_keys(DialogScope::UsageDashboard) {
            let shown = advertised.iter().any(|k| k.label == entry.label);
            for (code, mods) in entry.events {
                let mut s = state.clone();
                let outcome = decide(&mut s, KeyEvent::new(*code, *mods));
                if shown {
                    assert_ne!(
                        outcome,
                        UsageKeyOutcome::NotConsumed,
                        "card {card}: footer advertises {code:?} ({}) but the handler ignores it",
                        entry.label
                    );
                } else {
                    assert!(
                        !advertises(&state, *code),
                        "card {card}: {code:?} ({}) is filtered out but still advertises",
                        entry.label
                    );
                    assert_eq!(
                        outcome,
                        UsageKeyOutcome::NotConsumed,
                        "card {card}: footer hides {code:?} ({}) but the handler still consumes it",
                        entry.label
                    );
                }
            }
        }
        assert_eq!(
            advertised.iter().any(|k| k.label == "Enter"),
            card == CACHE_CARD,
            "card {card}: Enter is only reachable on the Cache Efficiency card"
        );
    }
}

/// Enter opens the breakdown on the Cache Efficiency card and nowhere else,
/// because nowhere else has a breakdown to open.
#[test]
fn enter_opens_the_breakdown_only_on_the_cache_card() {
    for card in 0..CARD_COUNT {
        let mut s = DashboardState {
            focused_card: card,
            ..Default::default()
        };
        let outcome = decide(&mut s, key(KeyCode::Enter));
        if card == CACHE_CARD {
            assert_eq!(outcome, UsageKeyOutcome::Consumed);
            assert!(s.cache_detail_open, "Enter must open the breakdown");
        } else {
            assert_eq!(outcome, UsageKeyOutcome::NotConsumed);
            assert!(!s.cache_detail_open, "no other card opens a breakdown");
        }
    }
}

/// While the breakdown is open the grid behind it is frozen: only Esc acts,
/// and it returns to the grid instead of leaving the dashboard.
#[test]
fn breakdown_swallows_the_grid_and_esc_backs_out() {
    let open = || DashboardState {
        focused_card: CACHE_CARD,
        cache_detail_open: true,
        ..Default::default()
    };

    let advertised = advertised_keys(&open());
    assert_eq!(
        advertised.len(),
        1,
        "an open breakdown advertises exactly one key, got {:?}",
        advertised.iter().map(|k| k.label).collect::<Vec<_>>()
    );
    assert_eq!(advertised[0].label, "Esc");

    for code in [
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Enter,
        KeyCode::Char('w'),
    ] {
        let mut s = open();
        assert_eq!(
            decide(&mut s, key(code)),
            UsageKeyOutcome::Consumed,
            "{code:?} must be swallowed, not passed on"
        );
        assert_eq!(
            s.focused_card, CACHE_CARD,
            "{code:?} moved the focus behind the breakdown"
        );
        assert!(
            s.cache_detail_open,
            "{code:?} closed the breakdown instead of doing nothing"
        );
    }

    let mut s = open();
    assert_eq!(decide(&mut s, key(KeyCode::Esc)), UsageKeyOutcome::Consumed);
    assert!(!s.cache_detail_open, "Esc backs out to the grid");
}

/// Esc means "back" in the breakdown and "leave" on the grid.
#[test]
fn esc_closes_the_breakdown_before_the_dashboard() {
    let mut grid = DashboardState::default();
    assert_eq!(decide(&mut grid, key(KeyCode::Esc)), UsageKeyOutcome::Close);

    let mut detail = DashboardState {
        cache_detail_open: true,
        ..Default::default()
    };
    assert_eq!(
        decide(&mut detail, key(KeyCode::Esc)),
        UsageKeyOutcome::Consumed
    );
    assert!(!detail.cache_detail_open);
    assert_eq!(
        decide(&mut detail, key(KeyCode::Esc)),
        UsageKeyOutcome::Close,
        "the second Esc leaves the dashboard"
    );
}

/// The period keys still map to their periods, upper case included.
#[test]
fn period_keys_map_to_their_periods() {
    for (code, period) in [
        (KeyCode::Char('t'), Period::Today),
        (KeyCode::Char('T'), Period::Today),
        (KeyCode::Char('w'), Period::Week),
        (KeyCode::Char('m'), Period::Month),
        (KeyCode::Char('a'), Period::AllTime),
    ] {
        let mut s = DashboardState::default();
        assert_eq!(decide(&mut s, key(code)), UsageKeyOutcome::Period(period));
    }
}

/// Tab and BackTab still walk the grid.
#[test]
fn tab_walks_the_grid() {
    let mut s = DashboardState::default();
    assert_eq!(decide(&mut s, key(KeyCode::Tab)), UsageKeyOutcome::Consumed);
    assert_eq!(s.focused_card, 1);
    assert_eq!(
        decide(&mut s, key(KeyCode::BackTab)),
        UsageKeyOutcome::Consumed
    );
    assert_eq!(s.focused_card, 0);
}
