//! AC-003 / AC-006: a process killed mid-turn must never leave torn storage.
//!
//! The journal's claim is a claim about *process death*, so the only honest way
//! to test it is to kill a process and reopen the file. This harness re-invokes
//! its own test binary as a child, lets the child walk a turn step by step, and
//! SIGKILLs it at one injection point. The parent then reopens the same database
//! file and asserts the invariant.
//!
//! Injection points, in turn order:
//!
//! | point | what already happened when the process dies |
//! |---|---|
//! | `after-open` | the turn row exists and nothing else |
//! | `after-intent` | the intent is on disk, the effect has not run |
//! | `after-effect` | the external effect ran, its result is not recorded |
//! | `after-settle` | the effect is settled, the turn is not committed |
//! | `after-commit` | the turn is settled |
//!
//! AC-003: after any of these, reopening storage yields a turn that is either
//! `running` (and so reconciled to `interrupted` at boot) or `committed`, and
//! never a committed turn whose effects are still unsettled.
//!
//! AC-006: when the process dies after the external effect ran but before the
//! result was recorded, exactly one external effect exists and its ledger row is
//! still readable under its idempotency key, so a resume can see that it landed.
//!
//! Scope, stated so a later reader does not over-read it: the ledger *consumer*
//! (the resume path that skips a re-issue) does not exist yet, so what is pinned
//! here is the storage contract that consumer will rely on. The counter file is
//! the stand-in for a side effect that cannot be rolled back.
//!
//! Gated to unix: the invariant is about `SIGKILL`, which Windows does not have,
//! so the whole harness compiles out there rather than pretending to test it.
#![cfg(unix)]

use crate::config::profile::with_home_override_async;
use crate::db::Database;
use crate::db::repository::turn::{TURN_COMMITTED, TURN_INTERRUPTED, TURN_RUNNING};
use crate::db::repository::{ToolExecutionRepository, TurnRepository};
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const POINT_ENV: &str = "OC_KILL_HARNESS_POINT";
const DB_ENV: &str = "OC_KILL_HARNESS_DB";
const COUNTER_ENV: &str = "OC_KILL_HARNESS_COUNTER";

/// The injection points, in the order a turn walks them.
const POINTS: [&str; 5] = [
    "after-open",
    "after-intent",
    "after-effect",
    "after-settle",
    "after-commit",
];

/// Die the way `kill -9` does: no unwinding, no `Drop`, no flush.
fn die_now() -> ! {
    // SAFETY: `getpid` and `kill` take no pointers and cannot corrupt memory.
    unsafe {
        libc::kill(libc::getpid(), libc::SIGKILL);
    }
    // Only reachable if the signal were blocked, which it is not.
    std::process::abort()
}

fn die_if(point: &str, at: &str) {
    if point == at {
        eprintln!("[harness] killing self at {at}");
        let _ = std::io::Write::flush(&mut std::io::stderr());
        die_now();
    }
}

/// The external effect: an append that cannot be rolled back.
fn record_external_effect(path: &Path) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open effect counter");
    writeln!(f, "effect").expect("append effect");
    f.sync_all().expect("fsync effect");
}

fn count_effects(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

async fn seed_session(db: &Database) -> Uuid {
    let id = Uuid::new_v4();
    let sid = id.to_string();
    db.pool()
        .get()
        .await
        .unwrap()
        .interact(move |conn| {
            conn.execute(
                "INSERT INTO sessions (id, title, created_at, updated_at, token_count, total_cost) \
                 VALUES (?1, 'kill-harness', strftime('%s','now'), strftime('%s','now'), 0, 0.0)",
                rusqlite::params![sid],
            )
        })
        .await
        .unwrap()
        .unwrap();
    id
}

/// The child half. Runs only when the parent set [`POINT_ENV`], so a normal
/// `cargo test` run treats it as a pass with no side effects.
#[tokio::test]
async fn kill_harness_child() {
    let Ok(point) = std::env::var(POINT_ENV) else {
        return;
    };
    let db_path = PathBuf::from(std::env::var(DB_ENV).expect("db path"));
    let counter = PathBuf::from(std::env::var(COUNTER_ENV).expect("counter path"));
    let home = db_path.parent().expect("db has a parent").to_path_buf();

    with_home_override_async(home, async move {
        let db = Database::connect(&db_path).await.expect("connect");
        db.run_migrations().await.expect("migrate");
        let session_id = seed_session(&db).await;
        let sid = session_id.to_string();
        let turns = TurnRepository::new(db.pool().clone());
        let effects = ToolExecutionRepository::new(db.pool().clone());

        let turn_id = turns.open(session_id).await.expect("open turn");
        die_if(&point, "after-open");

        let effect_id = Uuid::new_v4().to_string();
        effects
            .record_intent(
                &effect_id,
                &turn_id,
                "msg-1",
                &sid,
                "shell",
                &format!("{turn_id}:tool-1"),
                "hash-1",
            )
            .await
            .expect("record intent");
        die_if(&point, "after-intent");

        record_external_effect(&counter);
        die_if(&point, "after-effect");

        effects
            .settle(&effect_id, "success", Some("ok"), Some(1))
            .await
            .expect("settle");
        die_if(&point, "after-settle");

        turns.commit(&turn_id).await.expect("commit turn");
        die_if(&point, "after-commit");
    })
    .await;
}

/// `(turn id, turn state, unsettled effects, settled effects)` as read back
/// from disk after the child died.
async fn read_state(db: &Database) -> (Option<String>, Option<String>, i64, i64) {
    db.pool()
        .get()
        .await
        .unwrap()
        .interact(move |conn| {
            let id: Option<String> = conn
                .query_row("SELECT id FROM turns", [], |r| r.get(0))
                .optional()?;
            let state: Option<String> = conn
                .query_row("SELECT state FROM turns", [], |r| r.get(0))
                .optional()?;
            let pending: i64 = conn.query_row(
                "SELECT COUNT(*) FROM tool_executions WHERE committed_at IS NULL",
                [],
                |r| r.get(0),
            )?;
            let settled: i64 = conn.query_row(
                "SELECT COUNT(*) FROM tool_executions WHERE committed_at IS NOT NULL",
                [],
                |r| r.get(0),
            )?;
            Ok((id, state, pending, settled))
        })
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn a_kill_at_every_point_leaves_recoverable_storage() {
    let exe = std::env::current_exe().expect("test binary path");

    for point in POINTS {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".opencrabs");
        std::fs::create_dir_all(&home).unwrap();
        let db_path = home.join("opencrabs.db");
        let counter = tmp.path().join("effects.log");

        let status = std::process::Command::new(&exe)
            .args(["kill_harness_child", "--nocapture", "--test-threads=1"])
            .env(POINT_ENV, point)
            .env(DB_ENV, &db_path)
            .env(COUNTER_ENV, &counter)
            .env("HOME", tmp.path())
            .status()
            .expect("spawn child");
        assert!(
            !status.success(),
            "the child at '{point}' must die, not exit cleanly"
        );

        let db = Database::connect(&db_path).await.expect("reopen");
        let turns = TurnRepository::new(db.pool().clone());
        let effects = ToolExecutionRepository::new(db.pool().clone());
        let (turn_id, state, pending, settled) = read_state(&db).await;
        let turn_id = turn_id.unwrap_or_else(|| panic!("no turn row after a kill at '{point}'"));
        let state = state.unwrap_or_else(|| panic!("no turn state after a kill at '{point}'"));

        if state == TURN_COMMITTED {
            assert_eq!(
                pending, 0,
                "a committed turn must not leave an unsettled effect ('{point}')"
            );
        } else {
            assert_eq!(
                state, TURN_RUNNING,
                "a killed turn stays running until boot reconciles it ('{point}')"
            );
            let reconciled = turns.reconcile_running().await.unwrap();
            assert_eq!(
                reconciled.len(),
                1,
                "boot reconciles exactly the killed turn ('{point}')"
            );
            let (_, after, _, _) = read_state(&db).await;
            assert_eq!(
                after.as_deref(),
                Some(TURN_INTERRUPTED),
                "the reconciled turn is interrupted ('{point}')"
            );
        }

        match point {
            "after-open" => {
                assert_eq!((pending, settled), (0, 0), "nothing recorded yet");
                assert_eq!(count_effects(&counter), 0, "no effect ran");
            }
            "after-intent" => {
                assert_eq!((pending, settled), (1, 0), "intent on disk, unsettled");
                assert_eq!(count_effects(&counter), 0, "no effect ran yet");
            }
            "after-effect" => {
                // AC-006: the effect landed exactly once, and the ledger still
                // knows about it under its key, so a resume can see it instead
                // of replaying it.
                assert_eq!(count_effects(&counter), 1, "exactly one external effect");
                assert_eq!((pending, settled), (1, 0), "the result is not recorded");
                let open = effects.pending_for_turn(&turn_id).await.unwrap();
                assert_eq!(open.len(), 1, "one open ledger row for the effect");
                assert!(
                    open[0].effect_key.is_some(),
                    "the idempotency key survives the kill"
                );
            }
            "after-settle" => {
                assert_eq!((pending, settled), (0, 1), "the effect is settled");
                assert_eq!(count_effects(&counter), 1, "still one external effect");
            }
            "after-commit" => {
                assert_eq!((pending, settled), (0, 1), "settled before the commit");
                assert_eq!(count_effects(&counter), 1, "still one external effect");
            }
            other => panic!("unknown injection point {other}"),
        }
    }
}
