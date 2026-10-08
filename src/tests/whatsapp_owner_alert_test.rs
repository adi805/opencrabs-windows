//! Owner alerts for WhatsApp account events (#1999).
//!
//! A ban, an account lock or a connect failure has to reach a human even
//! though WhatsApp itself is the thing that is down. These tests pin the parts
//! that decide behaviour: which causes alert, what the message says, how a
//! repeated cause is deduplicated, and where the alert is addressed.
//!
//! `alert_owner` itself is not exercised here: it would need a live Telegram
//! bot. It is deliberately thin for exactly that reason, and everything that
//! carries a decision lives in the pure helpers below.

use std::time::{Duration, Instant};

use crate::channels::whatsapp::owner_alert::{
    ALERT_COOLDOWN, ban_text, claim_alert, connect_failure_name, connect_failure_needs_owner,
    connect_failure_text, human_duration, logged_out_text, owner_alert_chat,
};
use crate::config::Config;
use wacore::types::events::{ConnectFailureReason, TempBanReason};

/// A `Config` carrying only the Telegram section under test.
fn config_with(telegram: &str) -> Config {
    toml::from_str(&format!("[channels.telegram]\n{telegram}\n")).expect("test TOML must parse")
}

#[test]
fn claim_alert_is_once_per_cooldown() {
    let key = "test:claim-cooldown";
    let t0 = Instant::now();

    assert!(claim_alert(key, t0), "the first announcement is claimed");
    assert!(
        !claim_alert(key, t0 + Duration::from_secs(1)),
        "a repeat inside the cooldown stays quiet"
    );
    assert!(
        !claim_alert(key, t0 + ALERT_COOLDOWN - Duration::from_secs(1)),
        "still inside the cooldown one second before it expires"
    );
    assert!(
        claim_alert(key, t0 + ALERT_COOLDOWN),
        "the cooldown expiring re-arms the announcement"
    );
}

#[test]
fn claim_alert_is_scoped_per_cause() {
    let t0 = Instant::now();

    assert!(claim_alert("test:cause-a", t0));
    assert!(
        claim_alert("test:cause-b", t0),
        "a different cause is its own announcement, not a repeat"
    );
}

#[test]
fn claim_alert_survives_a_clock_that_went_backwards() {
    let key = "test:claim-clock";
    let t0 = Instant::now();

    assert!(claim_alert(key, t0));
    // `saturating_duration_since` keeps this from panicking, and the cause was
    // just announced, so it must stay quiet.
    assert!(!claim_alert(key, t0 - Duration::from_secs(5)));
}

#[test]
fn transient_connect_failures_are_logged_not_alerted() {
    // The client retries these by itself, so an alert per reconnect attempt
    // would be noise rather than signal.
    for reason in [
        ConnectFailureReason::ServiceUnavailable,
        ConnectFailureReason::InternalServerError,
    ] {
        assert!(
            !connect_failure_needs_owner(&reason),
            "{reason:?} is retried by the client and must not page the owner"
        );
    }
}

#[test]
fn account_state_failures_reach_the_owner() {
    for reason in [
        ConnectFailureReason::AccountLocked,
        ConnectFailureReason::TempBanned,
        ConnectFailureReason::LoggedOut,
        ConnectFailureReason::UnknownLogout,
        ConnectFailureReason::ClientOutdated,
        ConnectFailureReason::BadUserAgent,
        ConnectFailureReason::Unknown(4999),
    ] {
        assert!(
            connect_failure_needs_owner(&reason),
            "{reason:?} changes what the operator must do, so it must alert"
        );
    }
}

#[test]
fn connect_failure_text_names_the_reason_not_only_the_code() {
    let text = connect_failure_text(&ConnectFailureReason::BadUserAgent, Some("denied"));

    assert!(
        text.contains("409"),
        "the wire code stays greppable: {text}"
    );
    assert!(text.contains("user agent rejected"), "{text}");
    assert!(
        text.contains("denied"),
        "the server's own copy is kept: {text}"
    );
}

#[test]
fn unknown_connect_failure_codes_are_still_named() {
    assert_eq!(
        connect_failure_name(&ConnectFailureReason::Unknown(4999)),
        "unrecognised connect failure (wire 4999)"
    );
}

#[test]
fn human_duration_reads_like_a_duration() {
    assert_eq!(human_duration(Duration::from_secs(0)), "under a minute");
    assert_eq!(human_duration(Duration::from_secs(59)), "under a minute");
    assert_eq!(human_duration(Duration::from_secs(60)), "1m");
    assert_eq!(human_duration(Duration::from_secs(2700)), "45m");
    assert_eq!(human_duration(Duration::from_secs(3600)), "1h");
    assert_eq!(human_duration(Duration::from_secs(3600 + 1200)), "1h 20m");
    assert_eq!(human_duration(Duration::from_secs(86_400)), "1d");
    assert_eq!(
        human_duration(Duration::from_secs(86_400 + 4 * 3600)),
        "1d 4h"
    );
}

#[test]
fn ban_text_carries_reason_duration_and_appeal_link() {
    let text = ban_text(
        &TempBanReason::SentToTooManyPeople,
        Duration::from_secs(86_400),
        Some("You can't use WhatsApp right now"),
        Some("https://www.whatsapp.com/contact/?form=appeal"),
    );

    assert!(text.contains("temporarily banned"), "{text}");
    // The library's own copy says which heuristic tripped, which is the part an
    // operator can act on.
    assert!(
        text.contains("you sent too many messages to people"),
        "{text}"
    );
    assert!(text.contains("1d"), "{text}");
    assert!(
        text.contains("https://www.whatsapp.com/contact/?form=appeal"),
        "{text}"
    );
    assert!(text.contains("You can't use WhatsApp right now"), "{text}");
}

#[test]
fn ban_text_omits_absent_optional_fields() {
    let text = ban_text(
        &TempBanReason::BlockedByUsers,
        Duration::from_secs(300),
        None,
        None,
    );

    assert!(text.contains("5m"), "{text}");
    assert!(!text.contains("Server:"), "{text}");
    assert!(!text.contains("Appeal:"), "{text}");
}

#[test]
fn ban_text_ignores_blank_optional_fields() {
    let text = ban_text(
        &TempBanReason::BroadcastList,
        Duration::from_secs(600),
        Some("   "),
        Some(""),
    );

    assert!(!text.contains("Server:"), "{text}");
    assert!(!text.contains("Appeal:"), "{text}");
}

#[test]
fn logged_out_text_separates_a_lock_from_an_unlink() {
    let text = logged_out_text(
        true,
        &ConnectFailureReason::AccountLocked,
        Some("Your account was locked"),
        None,
    );

    assert!(text.contains("account locked"), "{text}");
    assert!(text.contains("at connect"), "{text}");
    assert!(
        text.contains("Do not re-pair"),
        "a lock must not read as a voluntary unlink: {text}"
    );
    assert!(text.contains("Your account was locked"), "{text}");
}

#[test]
fn logged_out_text_stays_plain_for_a_routine_logout() {
    let text = logged_out_text(false, &ConnectFailureReason::LoggedOut, None, None);

    assert!(text.contains("mid-session"), "{text}");
    assert!(
        !text.contains("Do not re-pair"),
        "lock-specific advice must not be given for a routine logout: {text}"
    );
}

#[test]
fn owner_alert_chat_prefers_an_explicit_bot_owner() {
    let cfg = config_with("enabled = true\nbot_owner = [\"4242\"]\nallowed_users = [\"1337\"]\n");

    assert_eq!(owner_alert_chat(&cfg), Some(4242));
}

#[test]
fn owner_alert_chat_falls_back_to_the_setup_user() {
    let cfg = config_with("enabled = true\nallowed_users = [\"1337\", \"4242\"]\n");

    assert_eq!(
        owner_alert_chat(&cfg),
        Some(1337),
        "the first allow-list entry is the operator who set the channel up"
    );
}

#[test]
fn owner_alert_chat_tolerates_a_leading_plus_and_skips_unusable_ids() {
    let cfg = config_with("enabled = true\nbot_owner = [\"@someone\", \"+4242\"]\n");

    assert_eq!(owner_alert_chat(&cfg), Some(4242));
}

#[test]
fn owner_alert_chat_is_none_when_telegram_is_disabled() {
    let cfg = config_with("enabled = false\nbot_owner = [\"4242\"]\n");

    assert_eq!(
        owner_alert_chat(&cfg),
        None,
        "an alert must not be addressed to a channel that is switched off"
    );
}

#[test]
fn owner_alert_chat_is_none_without_any_address() {
    let cfg = config_with("enabled = true\n");

    assert_eq!(owner_alert_chat(&cfg), None);
}
