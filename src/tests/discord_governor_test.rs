//! Unit suite for the Discord write-budget governor (PRD FR-003).
//!
//! Drives the REAL gate with time scripted: every evaluation reads the
//! injectable `gate_now` seam, and tests advance the virtual clock by hand
//! (`vclock::advance`) instead of sleeping. `#[tokio::test(start_paused = true)]`
//! means even the governor's internal holds cost zero wall time.
//!
//! Nothing here touches the live config: `Config::set_current` swaps an
//! in-memory mirror parsed from `config.toml.example`, never a file write.

use std::time::Duration;

use crate::channels::discord::governor;
use crate::channels::discord::governor::test_support as vclock;
use crate::channels::discord::governor::{Admission, WriteClass};
use crate::config::Config;

/// Swap the process-wide config mirror for `config.toml.example` plus
/// per-test `rate_limiter` mutations. Parsed fresh per test so knob changes
/// cannot leak sideways; [`vclock::registry_guard`] serializes the swap.
macro_rules! rl_config {
    ($($field:ident : $value:expr),* $(,)?) => {{
        let mut cfg: Config = toml::from_str(include_str!("../../config.toml.example"))
            .expect("embedded config.toml.example must parse");
        $(cfg.channels.discord.rate_limiter.$field = $value;)*
        Config::set_current(cfg);
    }};
}

const CH: u64 = 4_242;

#[tokio::test(start_paused = true)]
async fn bucket_refuses_when_spent_and_recovers_after_virtual_refill() {
    let _guard = vclock::registry_guard().await;
    vclock::reset(0);
    rl_config!(enabled: true, create_burst: 1, creates_per_5s: 1, max_hold_secs: 1);

    // Burst of one: the first write is admitted straight away.
    assert_eq!(
        governor::admit(CH, WriteClass::Create).await,
        Admission::Admit
    );

    // Bucket now empty and the wait (5 s at 1 per 5 s) exceeds the 1 s hold,
    // so a droppable class is refused rather than parked.
    assert_eq!(
        governor::admit(CH, WriteClass::Create).await,
        Admission::Drop
    );

    // A full refill window elapses on the virtual clock: the bucket is whole
    // again and the write goes through.
    vclock::advance(5_000);
    assert_eq!(
        governor::admit(CH, WriteClass::Create).await,
        Admission::Admit
    );

    let snap = governor::snapshot(CH).expect("channel was gated");
    assert_eq!(snap.creates, 2, "only the two admitted writes are counted");
    assert_eq!(snap.dropped, 1, "the refused write is counted as dropped");
}

#[tokio::test(start_paused = true)]
async fn final_class_waits_instead_of_being_dropped() {
    let _guard = vclock::registry_guard().await;
    vclock::reset(0);
    rl_config!(enabled: true, edit_burst: 1, edits_per_5s: 1, max_hold_secs: 1);

    assert_eq!(
        governor::admit(CH, WriteClass::Final).await,
        Admission::Admit
    );
    // Same spent bucket as the drop case above, but a settle render must land:
    // the governor holds for the refill instead of discarding the answer.
    assert_eq!(
        governor::admit(CH, WriteClass::Final).await,
        Admission::Admit
    );

    let snap = governor::snapshot(CH).expect("channel was gated");
    assert_eq!(snap.edits, 2);
    assert_eq!(snap.dropped, 0, "finals are never dropped");
    assert!(
        snap.throttled_ms >= 5_000,
        "the final waited out a refill window, got {} ms",
        snap.throttled_ms
    );
}

#[tokio::test(start_paused = true)]
async fn content_class_is_never_dropped() {
    let _guard = vclock::registry_guard().await;
    vclock::reset(0);
    rl_config!(enabled: true, edit_burst: 1, edits_per_5s: 1, max_hold_secs: 1);

    assert_eq!(
        governor::admit(CH, WriteClass::Final).await,
        Admission::Admit
    );
    assert_eq!(
        governor::admit(CH, WriteClass::Final).await,
        Admission::Admit
    );
    assert_eq!(governor::snapshot(CH).unwrap().dropped, 0);
}

#[tokio::test(start_paused = true)]
async fn cooldown_ladder_grows_on_consecutive_429s() {
    let _guard = vclock::registry_guard().await;
    vclock::reset(0);
    rl_config!(
        enabled: true,
        cooldown_base_millis: 1_000,
        cooldown_max_millis: 60_000
    );

    assert!(
        !governor::is_cooldown_active(CH),
        "no cooldown before any 429"
    );

    governor::record_429(CH, None);
    let first = governor::cooldown_remaining(CH).expect("cooldown armed");
    assert!(first <= Duration::from_millis(1_000) && first > Duration::from_millis(900));

    governor::record_429(CH, None);
    let second = governor::cooldown_remaining(CH).expect("cooldown re-armed");
    assert!(
        second <= Duration::from_millis(2_000) && second > Duration::from_millis(1_900),
        "second 429 doubles the wait, got {second:?}"
    );

    governor::record_429(CH, None);
    let third = governor::cooldown_remaining(CH).expect("cooldown re-armed");
    assert!(
        third <= Duration::from_millis(4_000) && third > Duration::from_millis(3_900),
        "third 429 doubles again, got {third:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn cooldown_is_capped_and_cleared_by_a_successful_write() {
    let _guard = vclock::registry_guard().await;
    vclock::reset(0);
    rl_config!(
        enabled: true,
        create_burst: 5,
        creates_per_5s: 5,
        cooldown_base_millis: 1_000,
        cooldown_max_millis: 4_000
    );

    // Hammer the ladder well past the cap; it must clamp, not grow forever.
    for _ in 0..12 {
        governor::record_429(CH, None);
    }
    let capped = governor::cooldown_remaining(CH).expect("cooldown armed");
    assert!(
        capped <= Duration::from_millis(4_000),
        "cooldown must clamp at the ceiling, got {capped:?}"
    );

    // Wait the window out, then a successful write clears the ladder.
    vclock::advance(4_100);
    assert!(!governor::is_cooldown_active(CH), "window elapsed");
    assert_eq!(
        governor::admit(CH, WriteClass::Create).await,
        Admission::Admit
    );
    assert!(
        !governor::is_cooldown_active(CH),
        "an admitted write resets the backoff ladder"
    );
}

#[tokio::test(start_paused = true)]
async fn discord_retry_after_wins_when_longer_than_the_ladder() {
    let _guard = vclock::registry_guard().await;
    vclock::reset(0);
    rl_config!(
        enabled: true,
        cooldown_base_millis: 1_000,
        cooldown_max_millis: 60_000
    );

    governor::record_429(CH, Some(Duration::from_secs(9)));
    let remaining = governor::cooldown_remaining(CH).expect("cooldown armed");
    assert!(
        remaining > Duration::from_millis(8_000),
        "Discord's own window is honoured when it exceeds the ladder, got {remaining:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn disabled_governor_admits_immediately() {
    let _guard = vclock::registry_guard().await;
    vclock::reset(0);
    rl_config!(enabled: false, create_burst: 0, creates_per_5s: 0);

    for _ in 0..5 {
        assert_eq!(
            governor::admit(CH, WriteClass::Create).await,
            Admission::Admit
        );
    }
    assert!(
        governor::snapshot(CH).is_none(),
        "a disabled governor must not even create target state"
    );
}

#[tokio::test(start_paused = true)]
async fn zero_refill_rate_fails_open_instead_of_wedging() {
    let _guard = vclock::registry_guard().await;
    vclock::reset(0);
    // A misconfigured zero budget can never mint a token. Failing open is the
    // deliberate choice: a governor that hangs the channel is worse than one
    // that overshoots.
    rl_config!(
        enabled: true,
        create_burst: 0,
        creates_per_5s: 0,
        edit_burst: 0,
        edits_per_5s: 0
    );

    assert_eq!(
        governor::admit(CH, WriteClass::Create).await,
        Admission::Admit
    );
    assert_eq!(
        governor::admit(CH, WriteClass::Final).await,
        Admission::Admit
    );
}

#[test]
fn parse_retry_after_reads_discord_payloads() {
    let payload = r#"{"message":"You are being rate limited.","retry_after":1.234,"global":false}"#;
    let parsed = governor::parse_retry_after(payload).expect("fractional seconds parse");
    assert!((parsed.as_secs_f64() - 1.234).abs() < 0.001);

    // Serenity sometimes hands back the window with a unit suffix.
    let suffixed = "429 rate limited, retry_after: 0.75";
    assert!((governor::parse_retry_after(suffixed).unwrap().as_secs_f64() - 0.75).abs() < 0.001);

    assert!(governor::parse_retry_after("no window here").is_none());
    assert!(governor::parse_retry_after("retry_after: notanumber").is_none());
}

#[test]
fn is_rate_limited_recognises_discord_429s() {
    assert!(governor::is_rate_limited("HTTP 429 Too Many Requests"));
    assert!(governor::is_rate_limited("You are being rate limited."));
    assert!(!governor::is_rate_limited("Unknown Message (10008)"));
}
