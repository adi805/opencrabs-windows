//! Tests for the first-time onboarding welcome message.
//!
//! Covers the `is_first_time` flag on `OnboardingWizard` and the
//! `WELCOME_MESSAGE` constant used in `dialogs.rs`.

use crate::tui::onboarding::{
    OnboardingWizard, WELCOME_MESSAGE, WELCOME_QUICK_START_MESSAGE, WizardMode, welcome_message_for,
};

// ── OnboardingWizard::is_first_time ─────────────────────────────

#[test]
fn wizard_is_first_time_defaults_to_false() {
    let w = OnboardingWizard::new();
    assert!(!w.is_first_time);
}

#[test]
fn wizard_is_first_time_can_be_set_true() {
    let mut w = OnboardingWizard::new();
    w.is_first_time = true;
    assert!(w.is_first_time);
}

#[test]
fn wizard_is_first_time_can_be_toggled_back() {
    let mut w = OnboardingWizard::new();
    w.is_first_time = true;
    assert!(w.is_first_time);
    w.is_first_time = false;
    assert!(!w.is_first_time);
}

// ── WELCOME_MESSAGE constant ────────────────────────────────────

#[test]
fn welcome_message_is_non_empty() {
    assert!(!WELCOME_MESSAGE.is_empty());
}

#[test]
fn welcome_message_contains_cron_manage() {
    assert!(
        WELCOME_MESSAGE.to_lowercase().contains("cron_manage"),
        "welcome message should mention cron_manage for task scheduling"
    );
}

#[test]
fn welcome_message_contains_heartbeat() {
    assert!(
        WELCOME_MESSAGE.to_lowercase().contains("heartbeat"),
        "welcome message should mention heartbeat"
    );
}

#[test]
fn welcome_message_contains_onboarding_complete() {
    assert!(
        WELCOME_MESSAGE
            .to_lowercase()
            .contains("onboarding complete"),
        "welcome message should contain 'onboarding complete'"
    );
}

#[test]
fn welcome_message_mentions_personality() {
    assert!(
        WELCOME_MESSAGE.contains("SOUL.md") && WELCOME_MESSAGE.contains("USER.md"),
        "welcome message should reference SOUL.md and USER.md for personality"
    );
}

#[test]
fn welcome_message_is_system_prompt() {
    assert!(
        WELCOME_MESSAGE.starts_with("[SYSTEM:"),
        "welcome message should start with [SYSTEM: to be hidden from user display"
    );
}

// ── welcome_message_for (quick start vs advanced) ───────────────

#[test]
fn quick_start_gets_the_interview_prompt() {
    assert_eq!(
        welcome_message_for(WizardMode::QuickStart),
        WELCOME_QUICK_START_MESSAGE
    );
}

#[test]
fn advanced_keeps_the_existing_welcome() {
    assert_eq!(welcome_message_for(WizardMode::Advanced), WELCOME_MESSAGE);
}

#[test]
fn quick_start_prompt_asks_about_the_user_and_fills_user_md() {
    let p = WELCOME_QUICK_START_MESSAGE;
    assert!(p.starts_with("[SYSTEM:"));
    assert!(p.contains("USER.md"));
    assert!(p.contains("write_opencrabs_file"));
    assert!(p.contains("operation=append"));
}

#[test]
fn quick_start_prompt_leaves_soul_alone_unless_asked() {
    assert!(WELCOME_QUICK_START_MESSAGE.contains("Do not rewrite SOUL.md"));
}

#[test]
fn quick_start_prompt_has_no_em_dash() {
    assert!(!WELCOME_QUICK_START_MESSAGE.contains('\u{2014}'));
}
