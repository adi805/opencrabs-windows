//! Discord plan-card tracking survives a restart (#104).
//!
//! Which message carries a session's plan card lived only in
//! [`DiscordState::plan_cards`], a process-local map. The field's own doc
//! comment claimed `PlanCardRepository` was "the durable backing and the same
//! rows Telegram reads and writes", but no repository was ever wired into the
//! Discord state: `set_plan_card` only inserted into the map, so the table
//! stayed empty (0 rows while a card was demonstrably live in the channel).
//!
//! A restart therefore lost the message id, and the card could be neither
//! edited (no tracked id) nor removed — the same defect #809 fixed for
//! Telegram, still open on the Discord path.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::channels::discord::DiscordState;
use crate::db::Database;
use crate::db::repository::PlanCardRepository;
use uuid::Uuid;

/// A fresh in-memory database, migrated, with a repository factory so each
/// "process" gets its own handle to the SAME store — which is what makes the
/// restart meaningful.
async fn migrated_db() -> Database {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    db
}

fn store_for(db: &Database) -> PlanCardRepository {
    PlanCardRepository::new(db.pool().clone())
}

/// A `DiscordState` standing in for one process run: same database, empty map.
async fn process(db: &Database) -> DiscordState {
    let state = DiscordState::new();
    state.set_plan_card_store(store_for(db)).await;
    state
}

/// The restart case: a later process must recover the same message id, or it
/// posts a SECOND card below the stale one and the old one can never be
/// removed (#104).
#[tokio::test]
async fn a_card_survives_a_restart() {
    let db = migrated_db().await;
    let session = Uuid::new_v4();

    let before = process(&db).await;
    before
        .set_plan_card(session, 987654321, 1234567890, "sig-1".to_string())
        .await;

    let after = process(&db).await;
    let (channel_id, message_id, signature) = after
        .plan_card(session)
        .await
        .expect("the card must survive a restart");

    assert_eq!(channel_id, 987654321);
    assert_eq!(message_id, 1234567890);
    assert_eq!(signature, "sig-1");
}

/// The signature is what suppresses no-op edits; losing it on restart meant the
/// first refresh always spent an API call rewriting identical content. It is
/// persisted alongside the id rather than rebuilt.
#[tokio::test]
async fn the_signature_survives_too() {
    let db = migrated_db().await;
    let session = Uuid::new_v4();

    let before = process(&db).await;
    before
        .set_plan_card(session, 111, 222, "rendered-body+keyboard".to_string())
        .await;

    let after = process(&db).await;
    assert_eq!(
        after.plan_card(session).await.unwrap().2,
        "rendered-body+keyboard"
    );
}

/// Clearing the card (discard, or the plan finished) must drop the durable row
/// as well — otherwise a restart resurrects a card the chat no longer shows.
#[tokio::test]
async fn clearing_untracks_durably() {
    let db = migrated_db().await;
    let session = Uuid::new_v4();

    let before = process(&db).await;
    before
        .set_plan_card(session, 111, 222, "sig".to_string())
        .await;
    before.clear_plan_card(session).await;

    let after = process(&db).await;
    assert!(
        after.plan_card(session).await.is_none(),
        "a cleared card must not come back after a restart"
    );
}

/// One card per session: re-tracking updates the row rather than adding a
/// second, which would reintroduce the duplicate this fixes — just in the
/// database instead of the chat.
#[tokio::test]
async fn re_tracking_updates_rather_than_duplicating() {
    let db = migrated_db().await;
    let session = Uuid::new_v4();

    let before = process(&db).await;
    before
        .set_plan_card(session, 111, 222, "sig-1".to_string())
        .await;
    before
        .set_plan_card(session, 111, 999, "sig-2".to_string())
        .await;

    let after = process(&db).await;
    let (_, message_id, signature) = after.plan_card(session).await.unwrap();
    assert_eq!(message_id, 999);
    assert_eq!(signature, "sig-2");
}

/// Sessions must not share a card: group channels run several at once, and one
/// clearing its card must not untrack another's.
#[tokio::test]
async fn sessions_do_not_share_a_card() {
    let db = migrated_db().await;
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());

    let before = process(&db).await;
    before.set_plan_card(a, 1, 111, "a".to_string()).await;
    before.set_plan_card(b, 2, 222, "b".to_string()).await;
    before.clear_plan_card(a).await;

    let after = process(&db).await;
    assert!(after.plan_card(a).await.is_none());
    assert_eq!(after.plan_card(b).await.unwrap().1, 222);
}

/// Control: with no store wired (a bare `DiscordState`, as the unit tests and
/// any non-DB surface construct it) the in-memory path must keep working and
/// never panic. The store is additive, not a new requirement.
#[tokio::test]
async fn without_a_store_tracking_stays_in_memory() {
    let state = DiscordState::new();
    let session = Uuid::new_v4();

    state.set_plan_card(session, 7, 8, "sig".to_string()).await;
    assert_eq!(state.plan_card(session).await.unwrap().1, 8);

    state.clear_plan_card(session).await;
    assert!(state.plan_card(session).await.is_none());
}
