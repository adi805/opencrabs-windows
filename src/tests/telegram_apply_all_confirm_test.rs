//! #2042: "Apply to all sessions" asks before it writes. The first tap is
//! `allq:` (count and Confirm/Cancel); only the Confirm button carries `allm:`,
//! the write. Cancel does nothing.

use crate::channels::commands::{
    apply_all_callback_data, apply_all_confirm_data, apply_all_prompt_text,
};

#[test]
fn the_first_tap_asks_and_does_not_carry_the_write() {
    let ask = apply_all_callback_data("claude-cli", "haiku-5-5").expect("fits");
    assert!(ask.starts_with("allq:"), "{ask}");
    assert!(!ask.starts_with("allm:"), "the first tap must not write");
}

#[test]
fn only_confirm_carries_the_write_and_both_fit_the_telegram_limit() {
    let confirm = apply_all_confirm_data("claude-cli", "haiku-5-5").expect("fits");
    assert_eq!(confirm, "allm:claude-cli|haiku-5-5");
    assert!(confirm.len() <= 64);
    assert_eq!(
        apply_all_callback_data("claude-cli", "haiku-5-5")
            .unwrap()
            .len(),
        confirm.len(),
        "ask and confirm are the same length"
    );
}

#[test]
fn an_over_long_pair_gets_no_button_rather_than_a_truncated_one() {
    let long = "m".repeat(80);
    assert_eq!(apply_all_callback_data("claude-cli", &long), None);
    assert_eq!(apply_all_confirm_data("claude-cli", &long), None);
}

#[test]
fn the_prompt_quotes_the_number_the_write_changes() {
    assert!(apply_all_prompt_text("p", "m", 0).contains("Nothing to change"));
    assert!(apply_all_prompt_text("p", "m", 1).contains("1 other session?"));
    assert!(apply_all_prompt_text("p", "m", 7).contains("7 other sessions?"));
}
