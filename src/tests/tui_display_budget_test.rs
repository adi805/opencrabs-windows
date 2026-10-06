//! Tests for `App::trim_messages_to_display_budget` (#1944). The walk is
//! newest-first and breaks at the first over-budget row, so a freshly
//! delivered answer bigger than the whole 200k display budget used to
//! exclude itself *and* everything older: the session switch reloaded the
//! DB, the trim dropped the report row behind the "N older messages
//! hidden" marker, and the answer the live view had just shown vanished.
//! The fix floors the trim at the newest row; these tests pin it.

use crate::db::models::Message;
use crate::tui::app::App;

fn tokens(s: &str) -> usize {
    crate::brain::tokenizer::count_tokens(s)
}

fn row(seq: i32, role: &str, content: &str) -> Message {
    Message {
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::nil(),
        role: role.to_string(),
        content: content.to_string(),
        sequence: seq,
        created_at: chrono::Utc::now(),
        token_count: None,
        cost: None,
        input_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        thinking: None,
        duration_secs: None,
    }
}

/// The #1944 shape: newest row alone busts the budget. It must still be
/// kept (hidden == 0 means no marker is inserted above it).
#[test]
fn newest_row_alone_over_budget_is_still_kept() {
    let big = "word ".repeat(2000);
    let big_tokens = tokens(&big);
    let budget = big_tokens - 1;
    let msgs = vec![
        row(1, "user", "audit the TUI"),
        row(2, "assistant", "working"),
        row(3, "assistant", &big),
    ];
    let (kept, hidden) = App::trim_messages_to_display_budget(&msgs, budget);
    assert_eq!(kept.len(), 1, "the delivered answer must survive the trim");
    assert_eq!(kept[0].sequence, 3, "the kept row must be the newest");
    assert_eq!(hidden, 2, "only the older rows are behind the marker");
}

/// A budget of zero still cannot produce an empty display for a non-empty
/// history: the newest row is the floor.
#[test]
fn zero_budget_keeps_newest_row() {
    let msgs = vec![row(1, "assistant", &"x ".repeat(500))];
    let (kept, hidden) = App::trim_messages_to_display_budget(&msgs, 0);
    assert_eq!(kept.len(), 1);
    assert_eq!(hidden, 0);
}

/// Empty input stays empty: the floor must not conjure a row.
#[test]
fn empty_history_trims_to_empty() {
    let (kept, hidden) = App::trim_messages_to_display_budget(&[], 200_000);
    assert!(kept.is_empty());
    assert_eq!(hidden, 0);
}

/// Normal behavior is unchanged: rows that fit are kept newest-first and
/// only the overflow is hidden (exactly 2 of 3 fit a 2-row budget).
#[test]
fn within_budget_trim_still_breaks_on_overflow() {
    let unit = tokens("x");
    let budget = unit * 2;
    let msgs = vec![
        row(1, "user", "x"),
        row(2, "user", "x"),
        row(3, "user", "x"),
    ];
    let (kept, hidden) = App::trim_messages_to_display_budget(&msgs, budget);
    assert_eq!(kept.len(), 2, "newest two rows fit the budget");
    assert_eq!(kept[0].sequence, 2);
    assert_eq!(kept[1].sequence, 3);
    assert_eq!(hidden, 1);
}
