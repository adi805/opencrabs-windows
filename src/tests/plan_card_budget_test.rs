//! Plan card height budget (#1750): on short terminals the card yields
//! rows before the chat viewport starves. Tall terminals keep the
//! pre-#1750 behaviour byte-identical.

use crate::tui::render::plan_card_height;

#[test]
fn tall_terminal_keeps_full_card() {
    // 40 rows: status 1 + input 10 + queue 2 + chat floor 6 = 19
    // reserved, 21 rows of budget left; desired 12 fits whole.
    assert_eq!(plan_card_height(12, 40, 10, 2), 12);
}

#[test]
fn short_pane_shrinks_card_to_budget() {
    // 24 rows: same 19 reserved, budget 5. The card gives up 7 rows
    // instead of crushing chat below its floor.
    assert_eq!(plan_card_height(12, 24, 10, 2), 5);
}

#[test]
fn degenerate_pane_holds_floor_of_three() {
    // Budget hits the floor on a tiny pane; the card keeps header +
    // one windowed row + footer so the plan stays identifiable.
    assert_eq!(plan_card_height(12, 8, 10, 2), 3);
}

#[test]
fn small_plan_untouched_when_budget_exceeds_desired() {
    // 5-task plan (desired 7) on a 24-row pane with a small input and
    // no queue: budget 14, desired wins.
    assert_eq!(plan_card_height(7, 24, 3, 0), 7);
}

#[test]
fn seed_strip_survives_normal_panes() {
    // The 5-line seed strip (#1945, text row padded by a blank above
    // and below) never grows and never shrinks on a sane pane: budget
    // 11, desired 5.
    assert_eq!(plan_card_height(5, 24, 4, 2), 5);
}

#[test]
fn seed_strip_degrades_to_card_min_on_tiny_pane() {
    // On a pane with no room left the strip still gets CARD_MIN rows
    // (borders + the text row; the widget renders it tight at height 3,
    // never dropping the message behind the padding).
    assert_eq!(plan_card_height(5, 8, 10, 2), 3);
}
