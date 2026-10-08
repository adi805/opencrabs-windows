//! FR-009 / AC-012: timeout and nickname from the moderation surface.
//!
//! Two actions join kick/ban/add_role on `discord_send`: `timeout` (a
//! communication timeout until an absolute instant) and `nickname`. Both are
//! plain `PATCH /guilds/{guild}/members/{user}` calls through
//! `Http::edit_member` (serenity 0.12.5, `src/http/client.rs:1924`) carrying an
//! `EditMember` body (`src/builder/edit_member.rs:15`).
//!
//! What these tests pin is the part that decides whether the request is
//! accepted, because that part is pure: the duration grammar and its ceiling,
//! the instant that reaches the wire, and the nickname rules Discord enforces
//! itself. The scope half pins the other failure mode: a scheduled job with no
//! `deliver_to` may not act on a guild member any more than it may post to a
//! channel it was never given.
//!
//! Nothing here touches the network. `edit_member` needs a live HTTP client and
//! a real guild, so the call itself is covered by the live capture (AC-012),
//! and the decisions in front of it are covered here.

use crate::brain::tools::discord_send::{
    parse_timeout_secs, timeout_until_rfc3339, validate_nickname,
};
use crate::cron::send_scope::{may_moderate, moderation_refusal, with_send_target};
use chrono::{TimeZone, Utc};

const SOMEWHERE: i64 = -1004428873948;

#[test]
fn a_compact_duration_is_read_into_seconds() {
    assert_eq!(parse_timeout_secs("30s").unwrap(), 30);
    assert_eq!(parse_timeout_secs("10m").unwrap(), 600);
    assert_eq!(parse_timeout_secs("2h").unwrap(), 7200);
    assert_eq!(parse_timeout_secs("7d").unwrap(), 604_800);
    // A bare count is seconds, which is what the schema advertises.
    assert_eq!(parse_timeout_secs("600").unwrap(), 600);
    // The unit letter is not case sensitive, and surrounding space is ignored.
    assert_eq!(parse_timeout_secs(" 10M ").unwrap(), 600);
}

#[test]
fn a_duration_past_the_platform_ceiling_is_refused_rather_than_clamped() {
    // Discord allows 28 days and rejects anything past it, so a longer request
    // is a mistake worth reporting, not a value to shorten silently.
    assert_eq!(parse_timeout_secs("28d").unwrap(), 2_419_200);
    let over = parse_timeout_secs("29d").unwrap_err();
    assert!(over.contains("28 days"), "got: {over}");
    let by_one_second = parse_timeout_secs("2419201").unwrap_err();
    assert!(by_one_second.contains("28 days"), "got: {by_one_second}");
    // The message has to name the alternative, or the caller just retries.
    assert!(over.contains("ban"), "got: {over}");
}

#[test]
fn a_duration_that_is_not_a_length_is_refused() {
    for spec in ["", "   ", "0", "0m", "-5m", "soon", "10 minutes", "1w"] {
        assert!(
            parse_timeout_secs(spec).is_err(),
            "'{spec}' should not read as a timeout"
        );
    }
    // An empty spec has its own message: it is a missing parameter, not a
    // malformed one.
    assert!(parse_timeout_secs("").unwrap_err().contains("duration"));
    assert!(parse_timeout_secs("0").unwrap_err().contains("above zero"));
}

#[test]
fn the_timeout_instant_is_an_absolute_rfc3339_stamp() {
    // `disable_communication_until` takes an ISO8601 string
    // (edit_member.rs:111), so the length has to become an instant before the
    // request is built. Seconds precision, UTC, trailing Z.
    let now = Utc.with_ymd_and_hms(2026, 10, 2, 1, 30, 0).unwrap();
    assert_eq!(timeout_until_rfc3339(600, now), "2026-10-02T01:40:00Z");
    assert_eq!(timeout_until_rfc3339(604_800, now), "2026-10-09T01:30:00Z");
}

#[test]
fn a_nickname_is_trimmed_and_bounded() {
    assert_eq!(validate_nickname(" Bob ").unwrap(), "Bob");

    // Exactly at the limit is accepted, and Discord counts characters rather
    // than bytes, so 32 multi-byte characters fit where 64 bytes would not.
    let exactly_32 = "x".repeat(32);
    let accepted = validate_nickname(&exactly_32).unwrap();
    assert_eq!(accepted.chars().count(), 32);
    let multibyte = "é".repeat(32);
    let multibyte_ok = validate_nickname(&multibyte).unwrap();
    assert_eq!(multibyte_ok.chars().count(), 32);

    let too_long = validate_nickname(&"x".repeat(33)).unwrap_err();
    assert!(too_long.contains("33"), "got: {too_long}");
    assert!(too_long.contains("32"), "got: {too_long}");
}

#[test]
fn an_empty_or_reserved_nickname_is_refused() {
    assert!(validate_nickname("").unwrap_err().contains("nickname"));
    assert!(validate_nickname("   ").unwrap_err().contains("nickname"));
    // Discord refuses these three outright; catching them here means the
    // failure names the reason instead of coming back as a 400.
    for reserved in ["everyone", "HERE", "Discord"] {
        let why = validate_nickname(reserved).unwrap_err();
        assert!(why.contains("reserved"), "{reserved} -> {why}");
    }
}

#[tokio::test]
async fn outside_a_job_moderation_is_not_restricted() {
    // The rule exists to stop a scheduled job acting outside its target, not to
    // police an ordinary turn.
    assert!(may_moderate());
}

#[tokio::test]
async fn a_job_with_a_destination_may_still_moderate() {
    with_send_target(Some(SOMEWHERE), async {
        assert!(may_moderate());
    })
    .await;
}

#[tokio::test]
async fn a_job_with_no_destination_may_not_moderate_anyone() {
    // Same authority question as a send: a job that named no deliver_to has no
    // channel to act through, so it cannot time out or rename a member either.
    with_send_target(None, async {
        assert!(!may_moderate());
        let why = moderation_refusal(4242);
        assert!(why.contains("4242"), "got: {why}");
        assert!(why.contains("deliver_to"), "got: {why}");
    })
    .await;
}
