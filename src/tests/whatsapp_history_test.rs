//! Pure decision logic for bounded WhatsApp history sync (#1525): request
//! planning, the 90-day capture window, per-chat opt-in matching, the
//! `from_me` anchor flag, and search-knob clamping. The caps here are the
//! decision doc's "constant with a test, not a vibe" clause
//! (~/.opencrabs/research/whatsapp-history-sync-decisions-1525.md); the DB
//! and transport legs they feed are thin enough that compilation covers them.

use crate::channels::whatsapp::history::{
    self, HISTORY_IMPORT_MAX_MESSAGES, from_me_of, in_window, opted_in, plan_import,
};
use chrono::{DateTime, TimeZone, Utc};

const NOW: i64 = 1_800_000_000;

fn ts(sec: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(sec, 0).unwrap()
}

#[test]
fn empty_store_anchors_a_full_page_at_now() {
    let plan = plan_import(None, ts(NOW)).expect("fresh store must ask for the newest head");
    assert_eq!(plan.oldest_msg_id, "");
    assert!(!plan.oldest_from_me);
    assert_eq!(plan.oldest_ts_ms, NOW * 1000);
    assert_eq!(plan.count, HISTORY_IMPORT_MAX_MESSAGES);
}

#[test]
fn in_window_anchor_continues_below_it() {
    let anchor = ts(NOW) - chrono::Duration::days(30);
    let plan = plan_import(Some(("MSG1", true, anchor)), ts(NOW)).unwrap();
    assert_eq!(plan.oldest_msg_id, "MSG1");
    assert!(plan.oldest_from_me);
    assert_eq!(plan.oldest_ts_ms, anchor.timestamp_millis());
    assert_eq!(plan.count, HISTORY_IMPORT_MAX_MESSAGES);
}

#[test]
fn anchor_at_or_beyond_the_90_day_cutoff_stops_the_walk() {
    let edge = ts(NOW) - chrono::Duration::days(history::HISTORY_IMPORT_MAX_AGE_DAYS);
    // At the cutoff the oldest stored row already reaches the bound; going
    // older only imports rows the capture window would drop anyway.
    assert!(plan_import(Some(("MSG1", false, edge)), ts(NOW)).is_none());
    assert!(
        plan_import(
            Some(("MSG1", false, edge - chrono::Duration::days(1))),
            ts(NOW)
        )
        .is_none()
    );
    // A day inside the cutoff still has room to pull.
    assert!(
        plan_import(
            Some(("MSG1", false, edge + chrono::Duration::days(1))),
            ts(NOW)
        )
        .is_some()
    );
}

#[test]
fn capture_window_filter_matches_the_bound() {
    let now = ts(NOW);
    assert!(in_window(now - chrono::Duration::days(89), now));
    assert!(!in_window(now - chrono::Duration::days(91), now));
}

#[test]
fn opt_in_is_exact_key_match() {
    let chats = vec!["123@s.whatsapp.net".to_string()];
    assert!(opted_in(&chats, "123@s.whatsapp.net"));
    // No prefix matching — #1544's trap is loose keys merging partitions.
    assert!(!opted_in(&chats, "1234@s.whatsapp.net"));
    assert!(!opted_in(&chats, "123"));
    assert!(!opted_in(&[], "123@s.whatsapp.net"));
}

#[test]
fn from_me_flag_is_conservative_without_owner() {
    assert!(from_me_of("+351900", Some("+351900")));
    assert!(!from_me_of("+351900", Some("+351911")));
    assert!(!from_me_of("+351900", None));
    assert!(!from_me_of("+351900", Some("")));
}

#[test]
fn search_knobs_clamp_to_the_feature_bounds() {
    assert_eq!(history::clamp_search(3650, 10_000), (90, 100));
    assert_eq!(history::clamp_search(0, 0), (1, 1));
    assert_eq!(history::clamp_search(7, 50), (7, 50));
}

// ── Empty-result message for the `whatsapp_history` tool ────────────────
//
// Separate concern from the sync decisions above: this is the answer a search
// gives when it matched nothing. The store keeps whichever JID WhatsApp sent
// (often `<lid>@lid`), so a search for the phone-number form of an active chat
// can miss — and a bare "no history" leaves the caller with no way to tell a
// LID mismatch from a genuinely empty chat.

use crate::brain::tools::whatsapp_history::empty_history_hint;

fn known(ids: &[(&str, i64)]) -> Vec<(String, i64)> {
    ids.iter().map(|(id, n)| ((*id).to_string(), *n)).collect()
}

#[test]
fn a_miss_names_the_stored_chat_ids_instead_of_dead_ending() {
    let ids = known(&[("210122852823201@lid", 29)]);
    let msg = empty_history_hint("6285270679623@s.whatsapp.net", 90, "", Some(&ids));
    assert!(
        msg.contains("210122852823201@lid (29)"),
        "the stored id must be named so the caller can retry, got {msg}"
    );
    assert!(
        msg.contains("@lid"),
        "the message must explain that LID-form ids exist, got {msg}"
    );
}

#[test]
fn a_genuinely_empty_store_says_so() {
    let msg = empty_history_hint("5511999999999@s.whatsapp.net", 30, "", Some(&[]));
    assert!(
        msg.contains("No WhatsApp chat has any stored messages yet"),
        "an empty store must be stated plainly, got {msg}"
    );
}

/// A failed chat listing must not be reported as "nothing is stored": that is
/// the same class of silent-wrong-answer this hint exists to remove.
#[test]
fn an_unreadable_store_does_not_claim_the_chat_is_empty() {
    let msg = empty_history_hint("5511999999999@s.whatsapp.net", 90, "", None);
    assert!(
        msg.contains("could not be read") && msg.contains("not proof"),
        "an unreadable list must be admitted, not read as empty, got {msg}"
    );
    assert!(
        !msg.contains("No WhatsApp chat has any stored messages yet"),
        "an unreadable list must not claim the store is empty, got {msg}"
    );
}

#[test]
fn the_query_and_window_are_echoed_back() {
    let msg = empty_history_hint("x@lid", 7, "invoice", Some(&[]));
    assert!(msg.contains("'invoice'"), "got {msg}");
    assert!(msg.contains("7 day(s)"), "got {msg}");
}

#[test]
fn the_listed_ids_are_capped() {
    let many: Vec<(String, i64)> = (0..25).map(|i| (format!("{i}@lid"), i as i64)).collect();
    let msg = empty_history_hint("x@s.whatsapp.net", 90, "", Some(&many));
    assert!(
        !msg.contains("24@lid"),
        "the hint must not echo an unbounded list, got {msg}"
    );
}
