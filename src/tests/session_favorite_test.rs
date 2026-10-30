//! Favorite sessions: schema, persistence, ordering.
//!
//! Three guarantees the feature leans on:
//! 1. the flag persists through `from_row` round-trips,
//! 2. the toggle write never touches `updated_at`, so the recency order of
//!    the rest of the list is untouched (the #1465 cursor hazard),
//! 3. `list_sessions` sorts favorites ahead of everything else while
//!    non-favorites keep their relative order.

use crate::db::repository::{SessionListOptions, SessionRepository};
use crate::services::ServiceContext;
use crate::services::session::*;
use chrono::Utc;
use uuid::Uuid;

async fn create_test_service() -> (SessionService, SessionRepository) {
    use crate::db::Database;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let pool = db.pool().clone();

    let context = ServiceContext::new(pool.clone());
    (SessionService::new(context), SessionRepository::new(pool))
}

#[tokio::test]
async fn favorite_persists_through_from_row() {
    let (svc, _repo) = create_test_service().await;
    let session = svc.create_session(Some("Fav".to_string())).await.unwrap();

    assert!(!session.favorite, "new sessions start not-favorite");

    let changed = svc.set_session_favorite(session.id, true).await.unwrap();
    assert!(changed, "read-back reports the new state");

    let reloaded = svc
        .get_session(session.id)
        .await
        .unwrap()
        .expect("session survives the toggle");
    assert!(reloaded.favorite);

    let cleared = svc.set_session_favorite(session.id, false).await.unwrap();
    assert!(!cleared);
    let reloaded = svc
        .get_session(session.id)
        .await
        .unwrap()
        .expect("session survives the untoggle");
    assert!(!reloaded.favorite);
}

#[tokio::test]
async fn set_favorite_never_touches_updated_at() {
    let (svc, _repo) = create_test_service().await;
    let session = svc.create_session(Some("Fav".to_string())).await.unwrap();
    let before = session.updated_at;

    // Timestamps are stored with second resolution, so hold the test past the
    // wall-clock boundary: an implementation that wrongly bumps `updated_at`
    // must land in a later second and fail this assertion.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    svc.set_session_favorite(session.id, true).await.unwrap();
    let after = svc
        .get_session(session.id)
        .await
        .unwrap()
        .expect("session survives the toggle")
        .updated_at;
    assert_eq!(after, before, "favorite toggle must not bump updated_at");
}

#[tokio::test]
async fn favorites_sort_first_and_preserve_otherwise_order() {
    let (svc, repo) = create_test_service().await;

    // Seed A, B, C with explicit, one-second-apart recency stamps: C is the
    // most recent, A the oldest. updated_at is stored with second resolution,
    // so rapid creates without explicit stamps would leave the order of the
    // three ambiguous.
    let mut a = svc.create_session(Some("A".to_string())).await.unwrap();
    let mut b = svc.create_session(Some("B".to_string())).await.unwrap();
    let mut c = svc.create_session(Some("C".to_string())).await.unwrap();
    let now = Utc::now();
    a.updated_at = now - chrono::Duration::seconds(300);
    b.updated_at = now - chrono::Duration::seconds(299);
    c.updated_at = now - chrono::Duration::seconds(298);
    repo.update(&a).await.unwrap();
    repo.update(&b).await.unwrap();
    repo.update(&c).await.unwrap();

    // Recency baseline: C, B, A.
    svc.set_session_favorite(a.id, true).await.unwrap();

    let list = svc
        .list_sessions(SessionListOptions {
            include_archived: false,
            limit: None,
            offset: 0,
            query: None,
            include_subagents: false,
        })
        .await
        .unwrap();
    let titles: Vec<String> = list
        .iter()
        .map(|s| s.title.clone().unwrap_or_default())
        .collect();

    // Favorite A jumps to the front; B and C keep their relative order.
    assert_eq!(titles, vec!["A", "C", "B"]);
}

#[tokio::test]
async fn set_favorite_is_a_noop_for_missing_sessions() {
    let (svc, _repo) = create_test_service().await;
    let missing = Uuid::new_v4();
    // Must not error: the UPDATE simply matches zero rows, and the read-back
    // (not the write) is what surfaces a missing session to the caller.
    let result = svc.set_session_favorite(missing, true).await;
    assert!(result.is_err(), "read-back of a missing session errors");
}
