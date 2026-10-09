//! Discord plan-card re-stick (#1880 follow-up): the settle tail that MOVES
//! the card to the bottom of the channel.
//!
//! The owner's report: on Discord the plan card stayed at the top and never
//! followed the conversation, unlike Telegram. Root cause: Discord only ever
//! edited the card in place (`refresh_plan_card`), while Telegram deletes the
//! tracked card and reposts it at the bottom on every settled turn
//! (`telegram/plan_card.rs::restick_plan_card_after_turn`). This module pins
//! the two seams the ported Discord tail must satisfy:
//!
//! 1. The re-stick draws from a per-channel sticky budget, so a burst of
//!    settles cannot churn the card toward Discord's write limits.
//! 2. A cardless turn spends NOTHING from that budget: the claim is gated on a
//!    tracked card, so a later re-stick is not starved (the Telegram #62
//!    lesson, carried over).
//!
//! Fixtures are synthetic and carry no user identifiers.

use std::path::Path;
use std::time::Duration;

use crate::channels::discord::DiscordState;

/// Flattened source: all whitespace removed, so a signature or a call split
/// across lines still matches a single-line needle.
fn flattened(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The budget admits one claim per interval, then refuses until it elapses.
/// Pinned against the real state object: a second re-stick moments after the
/// first must not fire, or rapid-fire settles delete and repost the card on
/// every turn.
#[test]
fn the_sticky_budget_admits_once_per_interval() {
    let state = DiscordState::new();
    let channel = 880_001u64;

    assert!(
        state.claim_sticky_action(channel, DiscordState::STICKY_STACK_MIN_INTERVAL),
        "the first re-stick in a channel must be admitted"
    );
    assert!(
        !state.claim_sticky_action(channel, DiscordState::STICKY_STACK_MIN_INTERVAL),
        "a second re-stick inside the interval must be refused"
    );

    // A DIFFERENT channel has its own budget: one channel's re-stick must not
    // starve another's.
    assert!(
        state.claim_sticky_action(880_002, DiscordState::STICKY_STACK_MIN_INTERVAL),
        "each channel draws from its own sticky budget"
    );

    std::thread::sleep(DiscordState::STICKY_STACK_MIN_INTERVAL + Duration::from_millis(5));
    assert!(
        state.claim_sticky_action(channel, DiscordState::STICKY_STACK_MIN_INTERVAL),
        "the budget must free up after the interval"
    );
}

/// The interval is the same 15 s Telegram uses, so the two surfaces cannot
/// drift into different flood-safety envelopes.
#[test]
fn the_interval_matches_telegrams() {
    assert_eq!(
        DiscordState::STICKY_STACK_MIN_INTERVAL,
        Duration::from_secs(15),
        "the Discord re-stick interval must mirror Telegram's 15 s budget"
    );
}

/// The tail deletes the tracked card BEFORE refreshing, and only when a card
/// is tracked AND the sticky claim is granted. Pinned from source as one
/// contiguous expression: reordering the two calls would leave the fresh post
/// above the stale one, and dropping the tracked-card gate would let a cardless
/// settle spend the budget.
#[test]
fn the_tail_deletes_then_refreshes_behind_the_tracked_card_gate() {
    let src = flattened("src/channels/discord/plan_card.rs");

    let gate = "ifstate.plan_card(session_id).await.is_some()&&state.claim_sticky_action(channel.get(),DiscordState::STICKY_STACK_MIN_INTERVAL){remove_plan_card(http,channel,state,session_id).await;}";
    assert!(
        src.contains(gate),
        "the re-stick must claim the sticky budget behind a tracked-card gate \
         and remove the old card first"
    );

    // Delete strictly precedes the refresh, or the fresh post is buried above
    // the card it was meant to replace.
    let delete_at = src
        .find("remove_plan_card(http,channel,state,session_id).await;")
        .expect("the tail must remove the tracked card");
    let refresh_at = src
        .find("refresh_plan_card(http,channel,state,session_id).await;")
        .expect("the tail must refresh after removing");
    assert!(
        delete_at < refresh_at,
        "remove_plan_card must run before refresh_plan_card"
    );
}
