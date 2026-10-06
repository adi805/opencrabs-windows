//! Unit tests for the governor's internals (#1211).
//!
//! Lifted out of `governor.rs`: the house rule is that no `#[cfg(test)] mod
//! tests` block lives in a source file — every suite is a file under
//! `src/tests/` registered in `mod.rs`. The end-to-end gate tests live in
//! `governor_gates_test`; this file covers the pure pieces those drive.

use std::time::{Duration, Instant};

use teloxide::types::ChatId;

use crate::channels::telegram::governor::{
    Bucket, Counters, EditClass, FinalDialect, chat_paused_until, ensure_bucket, format_summary,
    gate_now, is_permanent_edit_error, note_429_pause, test_support,
};

#[test]
fn bucket_allows_burst_then_throttles() {
    let mut b = Bucket::new(3, 1.0);
    let t0 = Instant::now();
    assert!(b.take(t0).is_ok());
    assert!(b.take(t0).is_ok());
    assert!(b.take(t0).is_ok());
    // Empty: refuses, and reports the full refill spacing.
    let err = b.take(t0).expect_err("bucket should be empty");
    assert_eq!(err, Duration::from_secs(1));
}

#[test]
fn bucket_refills_over_time_and_caps_at_capacity() {
    let mut b = Bucket::new(2, 1.0);
    let t0 = Instant::now();
    assert!(b.take(t0).is_ok());
    assert!(b.take(t0).is_ok());
    assert!(b.take(t0).is_err());
    // Halfway through one interval there is still no full token.
    assert!(b.take(t0 + Duration::from_millis(500)).is_err());
    // A full interval later the token is back.
    assert!(b.take(t0 + Duration::from_secs(1)).is_ok());
    // Idling far past capacity must not hoard tokens beyond the cap.
    let t1 = t0 + Duration::from_secs(60);
    assert!(b.take(t1).is_ok());
    assert!(b.take(t1).is_ok());
    assert!(b.take(t1).is_err());
}

#[test]
fn ensure_bucket_rebuilds_on_shape_change_keeps_on_match() {
    let mut slot = None;
    let rate = 0.5;
    ensure_bucket(&mut slot, 10, rate).take(Instant::now()).ok();
    // Same shape: the partially-consumed bucket survives.
    let kept = ensure_bucket(&mut slot, 10, rate);
    assert!(kept.tokens < f64::from(10u32));
    // Different shape: rebuilt fresh at full capacity.
    let fresh = ensure_bucket(&mut slot, 5, rate);
    assert!((fresh.tokens - 5.0).abs() < f64::EPSILON);
}

/// #635: a 429 pause freezes a bucket rather than emptying it — no refill for
/// the window, and no burst handed back the instant it lifts.
#[test]
fn bucket_pause_freezes_refill_and_reports_the_remaining_window() {
    let mut b = Bucket::new(3, 1.0);
    let t0 = Instant::now();
    for _ in 0..3 {
        assert!(b.take(t0).is_ok());
    }
    // Empty, and the next token is a full refill interval away.
    assert_eq!(b.take(t0).unwrap_err(), Duration::from_secs(1));

    // Paused for 10s: the take now reports the whole pause, not the refill.
    b.pause_arm(t0 + Duration::from_secs(10));
    assert_eq!(b.take(t0).unwrap_err(), Duration::from_secs(10));

    // Halfway through, the bucket has accrued nothing: a refill that ignored
    // the pause would have handed back five tokens by now.
    assert_eq!(
        b.take(t0 + Duration::from_secs(5)).unwrap_err(),
        Duration::from_secs(5)
    );

    // At the deadline the bucket is unfrozen but STILL EMPTY — the window is
    // dead time, not a windfall to spend on expiry.
    assert!(
        b.take(t0 + Duration::from_secs(10)).is_err(),
        "the pause must not refill the bucket the moment it expires"
    );
}

/// #635: a pause is not part of a bucket's shape, so a capacity/rate rebuild
/// (or replacing a `note_429_pause` placeholder) must carry it across.
#[test]
fn ensure_bucket_carries_a_live_pause_across_a_rebuild() {
    let mut slot = None;
    let t0 = Instant::now();
    ensure_bucket(&mut slot, 10, 0.5).pause_arm(t0 + Duration::from_secs(30));
    // Different shape: rebuilt fresh, but the pause is not shape state.
    let rebuilt = ensure_bucket(&mut slot, 5, 1.0);
    assert!((rebuilt.tokens - 5.0).abs() < f64::EPSILON);
    assert_eq!(
        rebuilt.take(t0).unwrap_err(),
        Duration::from_secs(30),
        "a rebuild must not hand a throttled chat its tokens back"
    );
}

/// #635: `note_429_pause` arms BOTH halves — the process-wide deadline and the
/// chat's own buckets — counts the arm, caps the window, and leaves DMs alone.
#[tokio::test]
async fn note_429_pause_arms_both_halves_caps_and_counts() {
    let _guard = test_support::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    test_support::reset(0);
    crate::channels::telegram::rate_limit::reset_global_cooldown();

    let chat = ChatId(-100_777);
    assert!(
        !chat_paused_until(chat.0, gate_now()).is_any(),
        "precondition: nothing holds this chat"
    );

    note_429_pause(chat.0, Duration::from_secs(9));
    let cd = chat_paused_until(chat.0, gate_now());
    assert!(cd.is_global(), "the process-wide deadline must be armed too");
    assert!(cd.chat_until.is_some(), "the chat's own pause must be armed");
    assert_eq!(
        test_support::snapshot(chat)
            .expect("note_429_pause creates the peer")
            .pause_armed_429,
        1,
        "each armed pause is counted on the peer"
    );

    // A multi-hour window is capped: the pacer never parks a chat for hours.
    // Read `now` ONCE — under the test clock `gate_now()` is real time plus an
    // offset, so two reads straddle a microsecond and the exact comparison
    // below would flake.
    test_support::advance(60_000);
    note_429_pause(chat.0, Duration::from_secs(3600));
    let now = gate_now();
    let capped = chat_paused_until(chat.0, now)
        .chat_until
        .expect("still paused")
        .duration_since(now);
    // The cap is 45s plus the 2s margin `note_429_pause` adds. `gate_now()` is
    // real time plus an offset, so a few microseconds elapse between arming the
    // pause and reading it back — compare with a tolerance, never exact equality.
    let tolerance = Duration::from_millis(50);
    assert!(
        capped >= Duration::from_secs(47) - tolerance
            && capped <= Duration::from_secs(47) + tolerance,
        "45s cap + 2s margin, got {capped:?}"
    );

    // DMs are ungoverned by construction: no peer state is created for them.
    note_429_pause(777, Duration::from_secs(9));
    assert!(test_support::snapshot(ChatId(777)).is_none());

    // Once the capped window elapses the chat is free again.
    test_support::advance(47_000);
    assert!(!chat_paused_until(chat.0, gate_now()).chat_until.is_some());

    crate::channels::telegram::rate_limit::reset_global_cooldown();
}

#[test]
fn ladder_order_drops_clock_first_and_final_never_drops() {
    let ladder = [
        EditClass::Clock,
        EditClass::BrainPreview,
        EditClass::Intermediary,
        EditClass::Status,
    ];
    for pair in ladder.windows(2) {
        assert!(
            pair[0].drop_rank() < pair[1].drop_rank(),
            "ladder must drop {pair:?} in ascending value order"
        );
    }
    // Final and Interactive outrank intermediate drops.
    assert_eq!(EditClass::Final.drop_rank(), 4);
    assert_eq!(EditClass::Interactive.drop_rank(), 5);
    let c = Counters {
        admitted_typing: 12,
        admitted_edits: 34,
        admitted_sends: 5,
        dropped_clock: 1,
        dropped_brain_preview: 2,
        dropped_intermediary: 3,
        dropped_status: 4,
        dropped_typing: 6,
        dropped_spacing: 0,
        queued_finals: 7,
        superseded_finals: 8,
        delivered_finals: 9,
        failed_finals: 10,
        throttled_typing_ms: 1500,
        throttled_send_ms: 2500,
        admitted_rich: 11,
        throttled_rich_ms: 3500,
        pause_armed_429: 3,
    };
    let line = format_summary(-100123, &c, 2, None).expect("active peer must summarize");
    assert!(line.contains("chat=-100123"));
    assert!(line.contains("admitted{typing=12,edits=34,sends=5,rich=11}"));
    assert!(
        line.contains(
            "dropped{clock=1,brain_preview=2,intermediary=3,status=4,typing=6,spacing=0}"
        )
    );
    assert!(line.contains("finals{queued=7,superseded=8,delivered=9,failed=10,pending=2}"));
    assert!(line.contains("throttled_ms{typing=1500,send=2500,rich=3500}"));
    assert!(line.contains("pause{pause_armed=3}"));
}

/// #635: a peer whose ONLY activity was a 429 pause still summarises — the
/// pause counter is part of `all_zero`, so a chat that took a throttle is not
/// silently omitted from the summary line.
#[test]
fn summary_reports_a_pause_only_peer() {
    let c = Counters {
        pause_armed_429: 1,
        ..Counters::default()
    };
    let line = format_summary(-100123, &c, 0, None).expect("a paused peer must summarize");
    assert!(line.contains("pause{pause_armed=1}"));
}

#[test]
fn permanent_edit_error_vocabulary_is_exact() {
    assert!(is_permanent_edit_error(
        "Telegram error: message to edit not found"
    ));
    assert!(is_permanent_edit_error(
        "Bad Request: message is not modified"
    ));
    assert!(!is_permanent_edit_error("Too Many Requests: retry after 3"));
    assert!(!is_permanent_edit_error("timeout"));
}

/// D1 (#171): Rate limiter defaults are sized safely below Telegram's ~20/min
/// group rate limit across edits, rich messages, and sends.
#[test]
fn rate_limiter_defaults_sized_below_group_limit() {
    let cfg = crate::config::RateLimiterConfig::default();
    assert_eq!(cfg.edits_per_minute, 18);
    assert_eq!(cfg.rich_per_minute, 18);
    assert_eq!(cfg.sends_ceiling_per_minute, 18);
}

/// #229: FinalDialect defaults to Html and supports Markdown variant.
#[test]
fn final_dialect_defaults_to_html() {
    assert_eq!(FinalDialect::default(), FinalDialect::Html);
    assert_ne!(FinalDialect::Html, FinalDialect::Markdown);
}

/// Tests for the process-wide proactive pacer & global 429 cooldown lock (#262).
#[tokio::test]
async fn global_pacer_burst_smoothing_and_cooldown() {
    let _guard = crate::channels::telegram::governor::test_support::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    crate::channels::telegram::governor::test_support::reset(0);
    crate::channels::telegram::rate_limit::reset_global_cooldown();

    // 1. Initial 25 permits should be granted immediately (burst capacity 25)
    for _ in 0..25 {
        assert!(
            crate::channels::telegram::governor::acquire_global_permit().await,
            "burst permits must be immediately granted"
        );
    }

    // 2. 26th permit requires waiting ~40ms (1 token at 25 req/s)
    let acquired = crate::channels::telegram::governor::acquire_global_permit().await;
    assert!(acquired, "26th permit must be granted after refill delay");

    // 3. Global 429 lock causes acquire_global_permit to wait full cooldown
    crate::channels::telegram::rate_limit::record_global_429(Duration::from_secs(5));
    assert!(crate::channels::telegram::rate_limit::is_global_cooldown_active());

    let waited = crate::channels::telegram::rate_limit::wait_global_cooldown().await;
    assert!(
        waited >= Duration::from_millis(6900) && waited <= Duration::from_millis(7100),
        "waited {waited:?} expected ~7s (5s + 2s margin)"
    );
    assert!(!crate::channels::telegram::rate_limit::is_global_cooldown_active());

    crate::channels::telegram::rate_limit::reset_global_cooldown();
}

#[tokio::test]
async fn global_cooldown_suppresses_drop_eligible_gates() {
    let _guard = crate::channels::telegram::governor::test_support::registry_guard().await;
    let _cooldown = crate::tests::telegram_cooldown_lock::guard().await;
    crate::channels::telegram::governor::test_support::reset(0);
    crate::channels::telegram::rate_limit::reset_global_cooldown();

    let chat = ChatId(-100123456789);
    crate::channels::telegram::governor::test_support::mark_forum(chat);

    // When global cooldown is active:
    crate::channels::telegram::rate_limit::record_global_429(Duration::from_secs(10));
    assert!(crate::channels::telegram::rate_limit::is_global_cooldown_active());

    // G1 typing must drop immediately without holding
    let typing_admitted =
        crate::channels::telegram::governor::admit_chat_action(chat, Some(1)).await;
    assert!(
        !typing_admitted,
        "typing must be dropped during global 429 cooldown"
    );

    // G2 cosmetic edit must drop immediately without consuming tokens
    let bot = teloxide::Bot::new("TESTTOKEN");
    let edit_admitted = crate::channels::telegram::governor::edit_admission(
        &bot,
        chat,
        teloxide::types::MessageId(100),
        EditClass::BrainPreview,
        "preview".to_string(),
        false,
    )
    .await;
    assert!(
        !edit_admitted,
        "cosmetic edit must be dropped during global 429 cooldown"
    );

    crate::channels::telegram::rate_limit::reset_global_cooldown();
}
