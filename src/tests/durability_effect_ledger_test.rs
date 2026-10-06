//! FR-003 crash semantics and wiring discipline for the tool effect ledger.
//!
//! Two things have to hold for the ledger to be worth anything:
//!
//! 1. The storage layer must not be able to represent a replayed effect. A
//!    resume that re-issues the same call has to find one row, not two, or the
//!    idempotency key buys nothing.
//! 2. The agent loop must actually commit the intent BEFORE the effect runs.
//!    That is a rule about the shape of the source, and a doc comment cannot
//!    fail a build, so the second half of this file reads the sources and turns
//!    a missing wire into a build failure.
//!
//! Scope, stated so a later reader does not over-read it: these tests pin the
//! ledger's contract and its call sites. They do NOT run a process that is
//! killed mid-turn. That end-to-end harness is tracked separately, because a
//! file-backed multi-process test joins the same class as the
//! `db_pre_migration_snapshot_test` stall and would add flakiness rather than
//! remove it.

use crate::db::Database;
use crate::db::repository::ToolExecutionRepository;
use std::path::Path;
use uuid::Uuid;

async fn make_db() -> Database {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    db
}

// ── the replay contract ─────────────────────────────────────────────

#[tokio::test]
async fn replaying_the_same_effect_key_never_writes_a_second_row() {
    let db = make_db().await;
    let repo = ToolExecutionRepository::new(db.pool().clone());
    // Same turn, same provider tool-use id: what a resume re-issues.
    let key = "turn-9:toolu_abc";

    repo.record_intent("eff-a", "turn-9", "msg-9", "sess-9", "git_push", key, "h1")
        .await
        .unwrap();
    repo.record_intent("eff-b", "turn-9", "msg-9", "sess-9", "git_push", key, "h1")
        .await
        .unwrap();

    let rows = repo.pending_for_turn("turn-9").await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a replayed effect must not open a second row"
    );
    assert_eq!(rows[0].id, "eff-a", "the original row survives the replay");
}

#[tokio::test]
async fn settling_the_original_row_leaves_the_replay_id_unknown() {
    let db = make_db().await;
    let repo = ToolExecutionRepository::new(db.pool().clone());
    let key = "turn-9:toolu_abc";
    repo.record_intent("eff-a", "turn-9", "msg-9", "sess-9", "git_push", key, "h1")
        .await
        .unwrap();
    repo.record_intent("eff-b", "turn-9", "msg-9", "sess-9", "git_push", key, "h1")
        .await
        .unwrap();

    assert_eq!(
        repo.settle("eff-a", "success", Some("pushed"), Some(12))
            .await
            .unwrap(),
        1,
        "the surviving row is the one that settles"
    );
    assert_eq!(
        repo.settle("eff-b", "success", None, None).await.unwrap(),
        0,
        "the replay's own id never existed, so it changes nothing"
    );
}

#[tokio::test]
async fn a_crash_mid_batch_leaves_only_the_unfinished_effects_pending() {
    let db = make_db().await;
    let repo = ToolExecutionRepository::new(db.pool().clone());

    repo.record_intent("e1", "t", "m", "s", "tool_a", "t:tu1", "h1")
        .await
        .unwrap();
    repo.record_intent("e2", "t", "m", "s", "tool_b", "t:tu2", "h2")
        .await
        .unwrap();
    // The first effect landed before the process died; the second never did.
    repo.settle("e1", "success", Some("done"), Some(5))
        .await
        .unwrap();

    let pending = repo.pending_for_turn("t").await.unwrap();
    assert_eq!(
        pending.len(),
        1,
        "only the effect that never settled reads as unknown"
    );
    assert_eq!(pending[0].effect_key.as_deref(), Some("t:tu2"));

    let landed = repo
        .find_by_effect_key("t:tu1")
        .await
        .unwrap()
        .expect("the landed effect is readable by its key");
    assert_eq!(landed.status, "success");
    assert!(
        landed.committed_at.is_some(),
        "a landed effect carries its commit stamp"
    );
}

// ── the wiring contract ─────────────────────────────────────────────

const SERVICE_DIR: &str = "src/brain/agent/service";
const LOOP_FILE: &str = "tool_loop.rs";
const PARALLEL_FILE: &str = "parallel_tools.rs";

/// A ledger intent committed before a tool runs.
const OPEN_NEEDLE: &str = "effect_ledger::open_effect(";
/// A ledger row settled after a tool returns.
const SETTLE_NEEDLE: &str = "effect_ledger::settle_effect(";
/// A tool actually being executed. Receiver-shaped, so it does not match the
/// registry's own definitions or its approval/`halts_turn` lookups.
const EXECUTE_NEEDLE: &str = "tool_registry.execute(&tool_name";

/// One service source file with all whitespace removed, so a call split across
/// lines still reads as one string.
fn flat(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(SERVICE_DIR)
        .join(name);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn every_tool_execution_site_opens_an_effect_first() {
    for name in [LOOP_FILE, PARALLEL_FILE] {
        let src = flat(name);
        let execs = src.matches(EXECUTE_NEEDLE).count();
        let opens = src.matches(OPEN_NEEDLE).count();
        assert!(
            execs > 0,
            "{name}: expected at least one tool execution site"
        );
        assert_eq!(
            opens, execs,
            "{name}: {execs} execution site(s) but {opens} ledger open(s); a tool can run \
             without committing its intent first"
        );
    }
}

#[test]
fn every_execution_site_can_settle_the_effect_it_opened() {
    for name in [LOOP_FILE, PARALLEL_FILE] {
        let src = flat(name);
        let execs = src.matches(EXECUTE_NEEDLE).count();
        let settles = src.matches(SETTLE_NEEDLE).count();
        assert!(
            settles >= execs,
            "{name}: {execs} execution site(s) but only {settles} settle site(s); an effect \
             would stay pending forever"
        );
    }
}

// ── the resume contract (FR-008) ────────────────────────────────────

/// Reconciling an interrupted turn must NOT clear its effect rows.
///
/// The `pending` row is the only record that a side effect's outcome is
/// unknown. If boot reconciliation wiped them, a resume would see a clean
/// slate and replay the call — the exact failure the ledger exists to
/// prevent. The turn flips to `interrupted`; the effects stay visible.
#[tokio::test]
async fn reconciling_a_turn_keeps_its_unknown_effects_visible() {
    use crate::db::repository::TurnRepository;

    let db = make_db().await;
    let turns = TurnRepository::new(db.pool().clone());
    let effects = ToolExecutionRepository::new(db.pool().clone());
    let sid = Uuid::new_v4();

    let turn_id = turns.open(sid).await.unwrap();
    effects
        .record_intent(
            "e1",
            &turn_id,
            "m",
            &sid.to_string(),
            "send_email",
            "k1",
            "h1",
        )
        .await
        .unwrap();
    effects
        .record_intent(
            "e2",
            &turn_id,
            "m",
            &sid.to_string(),
            "git_push",
            "k2",
            "h2",
        )
        .await
        .unwrap();
    // One landed before the process died.
    effects
        .settle("e1", "success", Some("sent"), Some(9))
        .await
        .unwrap();

    // Boot: the process that owned the turn is gone.
    let reconciled = turns.reconcile_running().await.unwrap();
    assert_eq!(reconciled.len(), 1, "the open turn is reconciled");

    let unknown = effects.pending_for_turn(&turn_id).await.unwrap();
    assert_eq!(
        unknown.len(),
        1,
        "reconciliation must not erase the unknown outcome; a resume has to be \
         able to tell 'landed' from 'unknown'"
    );
    assert_eq!(unknown[0].tool_name, "git_push");
}

// ── the replay contract, end to end through the wiring ──────────────

/// A replayed call must settle the row that EXISTS, not a fresh uuid.
///
/// `record_intent` is `INSERT OR IGNORE` against a unique `effect_key`, so the
/// second open of one call leaves the original row in place. If the handler
/// kept the uuid it generated for the replay, the settle would target a row
/// that was never written, return 0 rows changed, and leave the effect that
/// actually landed stuck `pending` forever: the ledger would then report an
/// effect as unknown exactly when it is known, and a resume would re-issue a
/// side effect that already happened.
#[tokio::test]
async fn a_replayed_open_returns_the_row_that_exists_so_its_settle_lands() {
    use crate::brain::agent::service::effect_ledger::open_effect_with;
    use crate::db::repository::ToolExecutionRepository;

    let db = make_db().await;
    let repo = ToolExecutionRepository::new(db.pool().clone());
    let input = serde_json::json!({ "cmd": "git push" });

    // First dispatch: the effect opens, then the process dies before settling.
    let first = open_effect_with(
        &repo, "turn-9", "msg-9", "sess-9", "git_push", "tu_abc", &input,
    )
    .await
    .expect("the first open returns a row id");

    // Resume: the provider re-issues the SAME tool-use id.
    let replay = open_effect_with(
        &repo, "turn-9", "msg-9", "sess-9", "git_push", "tu_abc", &input,
    )
    .await
    .expect("the replayed open still returns a row id");

    assert_eq!(
        replay, first,
        "the replay must resolve to the original row; a fresh uuid would settle nothing"
    );

    // The settle that follows the replayed open has to change the real row.
    assert_eq!(
        repo.settle(&replay, "success", Some("pushed"), Some(11))
            .await
            .unwrap(),
        1,
        "settling the resolved id must change exactly the row that exists"
    );
    let row = repo
        .find_by_effect_key("turn-9:tu_abc")
        .await
        .unwrap()
        .expect("the effect is readable by its key");
    assert_eq!(row.status, "success");
    assert_eq!(row.id, first, "the original row is the one that settled");

    // A distinct call in the same turn is a distinct effect, not a replay.
    let other = open_effect_with(
        &repo, "turn-9", "msg-9", "sess-9", "git_push", "tu_xyz", &input,
    )
    .await
    .expect("a different tool-use id opens its own row");
    assert_ne!(
        other, first,
        "a different tool-use id must not collapse onto the first effect"
    );
}
