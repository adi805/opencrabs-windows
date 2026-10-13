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
use crate::tests::source_shape::{flatten, read_in};
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

/// One service source file, flattened for needle matching. Delegates to the
/// shared helper so this file cannot drift from the other source-shape tests
/// (#197).
fn flat_source(name: &str) -> String {
    flatten(read_in(SERVICE_DIR, name))
}

#[test]
fn every_tool_execution_site_opens_an_effect_first() {
    for name in [LOOP_FILE, PARALLEL_FILE] {
        let src = flat_source(name);
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
        let src = flat_source(name);
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

/// A replayed call must be DECIDED, not blindly re-run (#213).
///
/// `record_intent` is `INSERT OR IGNORE` against a unique `effect_key`, so a
/// second open of one call leaves the original row in place. Returning that
/// row's id alone was the defect: the caller ran the effect anyway, then
/// settled a row that was already settled, so the ledger described the earlier
/// effect while a second one executed. The open now returns an
/// [`EffectDecision`] and only a `Fresh` claim may run.
#[tokio::test]
async fn a_settled_effect_key_is_not_executed_again() {
    use crate::brain::agent::service::effect_ledger::{
        EffectDecision, open_effect_with, should_execute,
    };
    use crate::db::repository::ToolExecutionRepository;

    let db = make_db().await;
    let repo = ToolExecutionRepository::new(db.pool().clone());
    let input = serde_json::json!({ "cmd": "git push" });

    // First dispatch: this caller wins the claim, so it may run the effect.
    let first = open_effect_with(
        &repo, "turn-9", "msg-9", "sess-9", "git_push", "tu_abc", &input,
    )
    .await;
    let first_id = match &first {
        EffectDecision::Fresh(id) => id.clone(),
        other => panic!("the first open must claim the key, got {other:?}"),
    };
    assert!(
        should_execute(&first),
        "a fresh claim is the one decision that may run the tool"
    );

    // Resume BEFORE the settle: the row exists and is still `pending`. The
    // ledger cannot prove the effect did not already land, so it must refuse
    // the replay instead of repeating a side effect.
    let pending = open_effect_with(
        &repo, "turn-9", "msg-9", "sess-9", "git_push", "tu_abc", &input,
    )
    .await;
    assert!(
        matches!(pending, EffectDecision::Pending(_)),
        "an unresolved key must read as pending, got {pending:?}"
    );
    assert!(
        !should_execute(&pending),
        "a pending effect must not be replayed: its outcome is unknown"
    );

    // The original owner settles it.
    assert_eq!(
        repo.settle(&first_id, "success", Some("pushed"), Some(11))
            .await
            .unwrap(),
        1,
        "the claim owner settles exactly the row it created"
    );

    // Resume AFTER the settle: same tool, same args. Reuse the recorded
    // outcome; never run the effect a second time.
    let landed = open_effect_with(
        &repo, "turn-9", "msg-9", "sess-9", "git_push", "tu_abc", &input,
    )
    .await;
    assert!(
        matches!(landed, EffectDecision::Landed(_)),
        "a settled key with matching identity must read as landed, got {landed:?}"
    );
    assert!(
        !should_execute(&landed),
        "a settled effect must not execute again"
    );
    assert_eq!(
        landed.settle_id(),
        None,
        "a landed effect owns no new row to settle"
    );

    // A distinct call in the same turn is a distinct effect, not a replay.
    let other = open_effect_with(
        &repo, "turn-9", "msg-9", "sess-9", "git_push", "tu_xyz", &input,
    )
    .await;
    assert!(
        matches!(&other, EffectDecision::Fresh(id) if id != &first_id),
        "a different tool-use id must open its own row, got {other:?}"
    );
}

/// A key reused for a different tool or different arguments is a collision,
/// not a replay, and must be refused BEFORE the effect runs (#213).
#[tokio::test]
async fn a_key_reused_with_different_tool_or_args_is_rejected() {
    use crate::brain::agent::service::effect_ledger::{
        EffectDecision, open_effect_with, should_execute,
    };
    use crate::db::repository::ToolExecutionRepository;

    let db = make_db().await;
    let repo = ToolExecutionRepository::new(db.pool().clone());

    let first = open_effect_with(
        &repo,
        "turn-7",
        "msg-7",
        "sess-7",
        "git_push",
        "tu_1",
        &serde_json::json!({ "cmd": "git push" }),
    )
    .await;
    assert!(matches!(first, EffectDecision::Fresh(_)));

    // Same key, different arguments.
    let other_args = open_effect_with(
        &repo,
        "turn-7",
        "msg-7",
        "sess-7",
        "git_push",
        "tu_1",
        &serde_json::json!({ "cmd": "git push --force" }),
    )
    .await;
    assert!(
        matches!(
            other_args,
            EffectDecision::Rejected {
                tool_differs: false,
                args_differ: true,
                ..
            }
        ),
        "same tool with different args must be rejected, got {other_args:?}"
    );
    assert!(
        !should_execute(&other_args),
        "a colliding key must never execute"
    );

    // Same key, different tool.
    let other_tool = open_effect_with(
        &repo,
        "turn-7",
        "msg-7",
        "sess-7",
        "git_reset",
        "tu_1",
        &serde_json::json!({ "cmd": "git push" }),
    )
    .await;
    assert!(
        matches!(
            other_tool,
            EffectDecision::Rejected {
                tool_differs: true,
                ..
            }
        ),
        "a different tool on the same key must be rejected, got {other_tool:?}"
    );
    assert!(!should_execute(&other_tool));
}

/// Every execution site must consult the decision, not just open the ledger.
///
/// This is the #213 invariant in source form: `open_effect` alone counted as
/// "protected" while the caller executed unconditionally, so the class could
/// return by adding a site that opens the row and ignores the verdict.
#[test]
fn every_execution_site_consults_the_ledger_decision() {
    for name in [LOOP_FILE, PARALLEL_FILE] {
        let src = flat_source(name);
        let execs = src.matches(EXECUTE_NEEDLE).count();
        let gates = src.matches("effect_ledger::should_execute(").count();
        assert!(
            gates >= execs,
            "{name}: {execs} execution site(s) but only {gates} should_execute gate(s); a tool \
             could run without asking the ledger whether the effect already landed"
        );
    }
}
