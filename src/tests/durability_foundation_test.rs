//! Durability foundation tests (milestone 2, FR-002..FR-005).
//!
//! These pin the data-layer contracts the Pi Durable invariants rest on: a
//! turn is open-or-settled and never torn, a submission id is idempotent, a
//! provider session id is stable across restarts, and an effect is recorded
//! as an intent before it runs and settled after. The wiring that drives them
//! from the agent loop is a later change; what is proven here is that the
//! storage layer cannot represent the broken states.

use crate::db::Database;
use crate::db::repository::submission::{SUBMISSION_DONE, SUBMISSION_RUNNING};
use crate::db::repository::turn::{TURN_COMMITTED, TURN_INTERRUPTED, TURN_RUNNING};
use crate::db::repository::{
    SessionIdentityRepository, SubmissionRepository, ToolExecutionRepository, TurnRepository,
};
use uuid::Uuid;

async fn make_db() -> Database {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    db
}

// ── FR-002: the turn journal ────────────────────────────────────────

#[tokio::test]
async fn turn_open_then_commit_settles_it() {
    let db = make_db().await;
    let repo = TurnRepository::new(db.pool().clone());
    let sid = Uuid::new_v4();

    let id = repo.open(sid).await.unwrap();
    let row = repo.find_by_id(&id).await.unwrap().expect("turn row");
    assert_eq!(row.state, TURN_RUNNING);
    assert!(
        row.committed_at.is_none(),
        "an open turn has no commit stamp"
    );
    assert_eq!(row.session_id, sid.to_string());

    repo.commit(&id).await.unwrap();
    let row = repo.find_by_id(&id).await.unwrap().unwrap();
    assert_eq!(row.state, TURN_COMMITTED);
    assert!(row.committed_at.is_some(), "a committed turn is stamped");
}

#[tokio::test]
async fn reconcile_marks_running_turns_interrupted_exactly_once() {
    let db = make_db().await;
    let repo = TurnRepository::new(db.pool().clone());
    let a = repo.open(Uuid::new_v4()).await.unwrap();
    let b = repo.open(Uuid::new_v4()).await.unwrap();

    let reconciled = repo.reconcile_running().await.unwrap();
    assert_eq!(reconciled.len(), 2, "both open turns are reconciled");
    for row in &reconciled {
        assert_eq!(
            row.state, TURN_INTERRUPTED,
            "the report carries the settled state"
        );
        assert!(row.error.is_some());
    }
    assert_eq!(
        repo.find_by_id(&a).await.unwrap().unwrap().state,
        TURN_INTERRUPTED
    );
    assert_eq!(
        repo.find_by_id(&b).await.unwrap().unwrap().state,
        TURN_INTERRUPTED
    );

    let again = repo.reconcile_running().await.unwrap();
    assert!(again.is_empty(), "a second boot finds nothing left running");
}

#[tokio::test]
async fn commit_cannot_resurrect_a_reconciled_turn() {
    let db = make_db().await;
    let repo = TurnRepository::new(db.pool().clone());
    let id = repo.open(Uuid::new_v4()).await.unwrap();
    repo.reconcile_running().await.unwrap();

    repo.commit(&id).await.unwrap();
    assert_eq!(
        repo.find_by_id(&id).await.unwrap().unwrap().state,
        TURN_INTERRUPTED,
        "a settled turn stays settled; commit only moves running -> committed"
    );
}

#[tokio::test]
async fn latest_turn_for_session_reads_the_newest() {
    let db = make_db().await;
    let repo = TurnRepository::new(db.pool().clone());
    let sid = Uuid::new_v4();
    let first = repo.open(sid).await.unwrap();
    repo.commit(&first).await.unwrap();
    let second = repo.open(sid).await.unwrap();

    let latest = repo.latest_for_session(sid).await.unwrap().expect("latest");
    assert_eq!(latest.id, second);
    assert_eq!(latest.state, TURN_RUNNING);
}

// ── FR-004: idempotent submissions ──────────────────────────────────

#[tokio::test]
async fn submission_claim_is_idempotent() {
    let db = make_db().await;
    let repo = SubmissionRepository::new(db.pool().clone());

    let (first, created_a) = repo.claim("req-1", "sess-1").await.unwrap();
    assert!(created_a, "the first claim owns the run");

    let (second, created_b) = repo.claim("req-1", "sess-1").await.unwrap();
    assert!(!created_b, "a known request id must not start a second run");
    assert_eq!(first.request_id, second.request_id);
    assert_eq!(
        first.created_at, second.created_at,
        "the existing row is returned untouched"
    );

    assert_eq!(repo.list_for_session("sess-1").await.unwrap().len(), 1);
}

#[tokio::test]
async fn concurrent_claims_of_one_id_produce_one_row() {
    let db = make_db().await;
    let repo = SubmissionRepository::new(db.pool().clone());

    let a = {
        let r = repo.clone();
        tokio::spawn(async move { r.claim("req-race", "sess-r").await })
    };
    let b = {
        let r = repo.clone();
        tokio::spawn(async move { r.claim("req-race", "sess-r").await })
    };
    let ra = a.await.unwrap().unwrap();
    let rb = b.await.unwrap().unwrap();

    let created = [ra.1, rb.1].iter().filter(|created| **created).count();
    assert_eq!(created, 1, "exactly one racing claim creates the row");
    assert_eq!(repo.list_for_session("sess-r").await.unwrap().len(), 1);
}

#[tokio::test]
async fn submission_state_moves_and_keeps_the_message_link() {
    let db = make_db().await;
    let repo = SubmissionRepository::new(db.pool().clone());
    repo.claim("req-2", "sess-2").await.unwrap();

    repo.set_state("req-2", SUBMISSION_RUNNING, None)
        .await
        .unwrap();
    assert_eq!(
        repo.find_by_request_id("req-2")
            .await
            .unwrap()
            .unwrap()
            .state,
        SUBMISSION_RUNNING
    );

    repo.set_state("req-2", SUBMISSION_DONE, Some("msg-9"))
        .await
        .unwrap();
    let row = repo.find_by_request_id("req-2").await.unwrap().unwrap();
    assert_eq!(row.state, SUBMISSION_DONE);
    assert_eq!(row.message_id.as_deref(), Some("msg-9"));

    // A later state change with no message id must not erase the link.
    repo.set_state("req-2", SUBMISSION_RUNNING, None)
        .await
        .unwrap();
    assert_eq!(
        repo.find_by_request_id("req-2")
            .await
            .unwrap()
            .unwrap()
            .message_id
            .as_deref(),
        Some("msg-9")
    );
}

#[tokio::test]
async fn unknown_submission_reads_none() {
    let db = make_db().await;
    let repo = SubmissionRepository::new(db.pool().clone());
    assert!(
        repo.find_by_request_id("never-claimed")
            .await
            .unwrap()
            .is_none()
    );
}

// ── FR-005: provider session identity ───────────────────────────────

#[tokio::test]
async fn provider_session_id_is_stable_and_replaceable() {
    let db = make_db().await;
    let repo = SessionIdentityRepository::new(db.pool().clone());

    let a = repo.ensure("sess-1").await.unwrap();
    let b = repo.ensure("sess-1").await.unwrap();
    assert_eq!(a, b, "ensure is stable across calls (restart-safe)");
    assert_eq!(
        repo.get("sess-1").await.unwrap().as_deref(),
        Some(a.as_str())
    );

    repo.set("sess-1", "forked-id").await.unwrap();
    assert_eq!(
        repo.get("sess-1").await.unwrap().as_deref(),
        Some("forked-id")
    );
    assert_eq!(
        repo.ensure("sess-1").await.unwrap(),
        "forked-id",
        "ensure keeps a fork's replacement instead of minting a new one"
    );
}

#[tokio::test]
async fn provider_session_ids_differ_per_session() {
    let db = make_db().await;
    let repo = SessionIdentityRepository::new(db.pool().clone());
    assert_ne!(
        repo.ensure("sess-a").await.unwrap(),
        repo.ensure("sess-b").await.unwrap()
    );
}

// ── FR-003: the effect ledger ───────────────────────────────────────

#[tokio::test]
async fn effect_intent_then_settle_records_one_landed_effect() {
    let db = make_db().await;
    let repo = ToolExecutionRepository::new(db.pool().clone());

    repo.record_intent(
        "eff-1", "turn-1", "msg-1", "sess-1", "git_push", "key-1", "hash-1",
    )
    .await
    .unwrap();

    let pending = repo.pending_for_turn("turn-1").await.unwrap();
    assert_eq!(
        pending.len(),
        1,
        "the intent is visible before the effect runs"
    );
    assert_eq!(pending[0].status, "pending");
    assert_eq!(pending[0].effect_key.as_deref(), Some("key-1"));
    assert!(pending[0].committed_at.is_none());

    let changed = repo
        .settle("eff-1", "success", Some("pushed"), Some(42))
        .await
        .unwrap();
    assert_eq!(changed, 1, "settling an open intent changes one row");
    assert!(repo.pending_for_turn("turn-1").await.unwrap().is_empty());

    let landed = repo
        .find_by_effect_key("key-1")
        .await
        .unwrap()
        .expect("landed effect");
    assert_eq!(landed.status, "success");
    assert_eq!(landed.result_preview.as_deref(), Some("pushed"));
    assert!(
        landed.committed_at.is_some(),
        "a settled effect carries a commit stamp"
    );
}

#[tokio::test]
async fn settling_twice_reports_no_second_change() {
    let db = make_db().await;
    let repo = ToolExecutionRepository::new(db.pool().clone());
    repo.record_intent(
        "eff-2", "turn-2", "msg-2", "sess-2", "send", "key-2", "hash-2",
    )
    .await
    .unwrap();

    assert_eq!(
        repo.settle("eff-2", "success", None, None).await.unwrap(),
        1
    );
    assert_eq!(
        repo.settle("eff-2", "success", None, None).await.unwrap(),
        0,
        "an already-settled effect is never settled twice"
    );
}

#[tokio::test]
async fn unknown_effect_key_reads_none() {
    let db = make_db().await;
    let repo = ToolExecutionRepository::new(db.pool().clone());
    assert!(
        repo.find_by_effect_key("no-such-key")
            .await
            .unwrap()
            .is_none()
    );
}
