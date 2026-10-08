//! Tests for the Bot API 10.3 stop button (`stopped_message_generation`).
//!
//! teloxide-core 0.13 has no variant for this update, so the typed parse turns
//! it into `UpdateKind::Error` and the dispatcher drops it in silence. The
//! raw poll loop therefore reads it off the envelope and parks it in a bounded,
//! time-limited lot that the turn's own streaming edit loop drains. These
//! tests pin the payload reader, the lot's bounds, the topic matching, and the
//! expiry that stops a stray press from cancelling the chat's NEXT turn.

use crate::channels::telegram::raw_updates::{
    STOP_CAP, STOP_TTL, STOPPED_GENERATION_KEY, age_generation_stops, clear_generation_stops,
    generation_stops_pending, raw_update_kind_name, stash_generation_stop,
    stopped_message_generation, take_generation_stop,
};
use serde_json::{Value, json};

/// A raw update shaped like the real thing: envelope id plus the 10.3 field.
fn stop_update(chat_id: i64, thread: Option<i32>, draft_id: i64) -> Value {
    let mut stop = json!({
        "chat": {"id": chat_id, "type": "private"},
        "draft_id": draft_id,
    });
    if let Some(t) = thread {
        stop["message_thread_id"] = json!(t);
    }
    json!({"update_id": 77, STOPPED_GENERATION_KEY: stop})
}

/// Park one stop, run the assertions, then hand the global lot back.
fn with_lot<T>(f: impl FnOnce() -> T) -> T {
    clear_generation_stops();
    let out = f();
    clear_generation_stops();
    out
}

// ── stopped_message_generation: payload reader ───────────────────────

#[test]
fn reads_a_dm_stop_payload() {
    let stop = stopped_message_generation(&stop_update(8910648287, None, 5))
        .expect("a payload with a chat and a draft id is a stop request");
    assert_eq!(stop.chat_id, 8910648287);
    assert_eq!(stop.message_thread_id, None);
    assert_eq!(stop.draft_id, 5);
}

#[test]
fn reads_a_forum_stop_payload_with_its_thread() {
    let stop = stopped_message_generation(&stop_update(-100200, Some(42), 9))
        .expect("a topic stop is still a stop request");
    assert_eq!(stop.chat_id, -100200);
    assert_eq!(stop.message_thread_id, Some(42));
    assert_eq!(stop.draft_id, 9);
}

#[test]
fn a_null_thread_is_read_as_no_thread() {
    let mut u = stop_update(-100200, Some(42), 9);
    u[STOPPED_GENERATION_KEY]["message_thread_id"] = Value::Null;
    let stop = stopped_message_generation(&u).expect("null thread is still usable");
    assert_eq!(stop.message_thread_id, None);
}

#[test]
fn an_ordinary_update_is_not_a_stop() {
    let u = json!({"update_id": 1, "message": {"message_id": 10, "text": "hi"}});
    assert!(stopped_message_generation(&u).is_none());
}

#[test]
fn an_unusable_payload_is_not_a_stop() {
    // No chat: nothing to cancel against.
    let no_chat = json!({"update_id": 1, STOPPED_GENERATION_KEY: {"draft_id": 5}});
    assert!(stopped_message_generation(&no_chat).is_none());
    // No draft id: the Bot API documents it as required on this class.
    let no_draft = json!({
        "update_id": 1,
        STOPPED_GENERATION_KEY: {"chat": {"id": 1}},
    });
    assert!(stopped_message_generation(&no_draft).is_none());
}

// ── the parking lot: bounds ──────────────────────────────────────────

#[test]
fn a_parked_stop_is_drained_once() {
    with_lot(|| {
        let stop = stopped_message_generation(&stop_update(1, None, 5)).expect("parses");
        assert!(stash_generation_stop(stop));
        assert_eq!(generation_stops_pending(), 1);

        let taken = take_generation_stop(1, None).expect("the parked stop is there");
        assert_eq!(taken.draft_id, 5);
        // Consumed, not peeked: a second drain must not re-cancel the turn.
        assert!(take_generation_stop(1, None).is_none());
        assert_eq!(generation_stops_pending(), 0);
    });
}

#[test]
fn the_lot_is_bounded_and_reports_its_overflow() {
    with_lot(|| {
        for i in 0..STOP_CAP {
            let stop =
                stopped_message_generation(&stop_update(i as i64, None, i as i64)).expect("parses");
            assert!(stash_generation_stop(stop), "entry {i} still fits");
        }
        assert_eq!(generation_stops_pending(), STOP_CAP);

        let extra = stopped_message_generation(&stop_update(9999, None, 9999)).expect("parses");
        assert!(
            !stash_generation_stop(extra),
            "a full lot reports the drop instead of failing silently"
        );
        assert_eq!(generation_stops_pending(), STOP_CAP, "cap holds");

        // The oldest went, the newest stayed: the lot evicts from the front.
        assert!(take_generation_stop(0, None).is_none(), "chat 0 was evicted");
        assert!(take_generation_stop(9999, None).is_some(), "chat 9999 is there");
    });
}

// ── the parking lot: which turn drains it ────────────────────────────

#[test]
fn an_exact_topic_match_wins_over_a_threadless_one() {
    with_lot(|| {
        let general = stopped_message_generation(&stop_update(-100200, None, 1)).expect("parses");
        let topic = stopped_message_generation(&stop_update(-100200, Some(42), 2)).expect("parses");
        stash_generation_stop(general);
        stash_generation_stop(topic);

        let taken = take_generation_stop(-100200, Some(42)).expect("the topic stop is preferred");
        assert_eq!(taken.draft_id, 2, "the topic-scoped turn takes its own stop");
        assert_eq!(generation_stops_pending(), 1, "the general stop is untouched");
    });
}

#[test]
fn a_threadless_stop_reaches_any_turn_in_the_chat() {
    with_lot(|| {
        let general = stopped_message_generation(&stop_update(-100200, None, 1)).expect("parses");
        stash_generation_stop(general);

        // A stop pressed in the General topic carries no thread, but the turn
        // that owns it may be a topic turn; the fallback is what connects them.
        let taken = take_generation_stop(-100200, Some(42)).expect("fallback match");
        assert_eq!(taken.draft_id, 1);
    });
}

#[test]
fn a_stop_for_another_chat_is_left_alone() {
    with_lot(|| {
        let stop = stopped_message_generation(&stop_update(1, None, 5)).expect("parses");
        stash_generation_stop(stop);
        assert!(take_generation_stop(2, None).is_none());
        assert_eq!(generation_stops_pending(), 1, "still parked for its own chat");
    });
}

// ── the parking lot: expiry ──────────────────────────────────────────

#[test]
fn an_expired_stop_does_not_cancel_the_next_turn() {
    with_lot(|| {
        let stop = stopped_message_generation(&stop_update(1, None, 5)).expect("parses");
        stash_generation_stop(stop);
        age_generation_stops(STOP_TTL + std::time::Duration::from_secs(1));

        assert!(
            take_generation_stop(1, None).is_none(),
            "a stop older than the TTL must not kill work the user never asked to stop"
        );
        assert_eq!(generation_stops_pending(), 0, "the expired entry is gone");
    });
}

#[test]
fn a_stop_inside_the_ttl_still_fires() {
    with_lot(|| {
        let stop = stopped_message_generation(&stop_update(1, None, 5)).expect("parses");
        stash_generation_stop(stop);
        age_generation_stops(STOP_TTL - std::time::Duration::from_secs(10));
        assert!(take_generation_stop(1, None).is_some(), "still inside the TTL");
    });
}

#[test]
fn parking_prunes_expired_entries_before_the_cap_can_bite() {
    with_lot(|| {
        for i in 0..STOP_CAP {
            let stop =
                stopped_message_generation(&stop_update(i as i64, None, i as i64)).expect("parses");
            stash_generation_stop(stop);
        }
        age_generation_stops(STOP_TTL + std::time::Duration::from_secs(1));

        // A full lot of corpses must not report a drop: the prune runs first.
        let fresh = stopped_message_generation(&stop_update(9999, None, 9999)).expect("parses");
        assert!(stash_generation_stop(fresh), "expired entries are not live capacity");
        assert_eq!(generation_stops_pending(), 1);
    });
}

// ── raw_update_kind_name ─────────────────────────────────────────────

#[test]
fn names_the_stop_update_before_the_typed_parse() {
    assert_eq!(
        raw_update_kind_name(&stop_update(1, None, 5)),
        STOPPED_GENERATION_KEY
    );
}

#[test]
fn names_a_known_update_from_its_envelope() {
    let u = json!({"update_id": 1, "message": {"message_id": 10}});
    assert_eq!(raw_update_kind_name(&u), "message");
    let cb = json!({"update_id": 2, "callback_query": {"id": "1"}});
    assert_eq!(raw_update_kind_name(&cb), "callback_query");
}

#[test]
fn falls_back_to_unknown() {
    let u = json!({"update_id": 1, "business_connection": {"id": "b"}});
    assert_eq!(raw_update_kind_name(&u), "unknown");
}
