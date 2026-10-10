//! #1925 regression: a scheduler whose loop never ran must not be silent.
//!
//! The #1893 escalation counts `tick()` returns inside `run()`: it needs the
//! loop to exist. A lock denial, a panic before the loop, or a spawn that was
//! never reached produced no counter, no notice and no log line at all. The
//! loop now stamps process-wide liveness instants and the daemon health
//! endpoint reports the verdict, so a surface that already polls `/health`
//! sees "no cron tick completed" without OpenCrabs owning a new timer.

use crate::cron::scheduler::{CRON_STALE_AFTER_SECS, CronLiveness, cron_liveness};
use crate::db::CronJobRepository;
use crate::db::Database;
use crate::db::models::CronJob;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

const STALE: u64 = CRON_STALE_AFTER_SECS;

fn make_job(name: &str) -> CronJob {
    CronJob::new(
        name.to_string(),
        "0 9 * * *".to_string(),
        "UTC".to_string(),
        "Test prompt".to_string(),
        None,
        None,
        "off".to_string(),
        true,
        None,
        None,
    )
}

// ── pure liveness decision ──────────────────────────────────────────────

#[test]
fn no_scheduler_and_no_enabled_jobs_is_idle() {
    assert_eq!(cron_liveness(1_000, 0, 0, 0), CronLiveness::Idle);
}

#[test]
fn enabled_jobs_without_a_scheduler_is_the_1925_silence() {
    // The exact reported gap: jobs exist that will never fire, and until now
    // nothing anywhere could say so.
    assert_eq!(
        cron_liveness(1_000, 0, 0, 3),
        CronLiveness::NotRunningWithJobs { enabled: 3 }
    );
}

#[test]
fn startup_without_a_first_tick_sits_in_the_grace_window() {
    // A loop that just started has no completed tick yet; the window applies
    // from spawn, so a fresh process is not called stalled.
    assert_eq!(
        cron_liveness(1_000 + STALE - 1, 1_000, 0, 1),
        CronLiveness::Healthy {
            age_secs: STALE - 1
        }
    );
}

#[test]
fn a_loop_that_never_completes_its_first_tick_goes_stale() {
    assert_eq!(
        cron_liveness(1_000 + STALE + 1, 1_000, 0, 1),
        CronLiveness::Stalled {
            age_secs: STALE + 1,
            enabled: 1
        }
    );
}

#[test]
fn stopped_ticks_are_stalled() {
    let now = 100_000;
    assert_eq!(
        cron_liveness(now, 50_000, now - STALE - 1, 2),
        CronLiveness::Stalled {
            age_secs: STALE + 1,
            enabled: 2
        }
    );
}

#[test]
fn a_tick_exactly_at_the_boundary_is_still_healthy() {
    // Strict `>`: the window says "older than 10 minutes", not "at least".
    let now = 100_000;
    assert_eq!(
        cron_liveness(now, 50_000, now - STALE, 2),
        CronLiveness::Healthy { age_secs: STALE }
    );
}

#[test]
fn a_recent_tick_is_healthy_with_its_age() {
    let now = 100_000;
    assert_eq!(
        cron_liveness(now, 50_000, now - 61, 0),
        CronLiveness::Healthy { age_secs: 61 }
    );
}

// ── the run() loop stamping (structural, #1893 precedent) ───────────────

#[test]
fn run_loop_stamps_liveness_in_place() {
    // The loop cannot be driven in a unit test (it sleeps 60s forever), so
    // its stamping shape is pinned here, like cron_silent_death_test does.
    let src = include_str!("../cron/scheduler.rs");
    let at = src
        .find("pub async fn run(self) {")
        .expect("run loop present");
    let head_end = src[at..]
        .find("tokio::time::sleep")
        .map(|i| at + i)
        .expect("loop sleep");
    let head = &src[at..head_end];
    assert!(
        head.contains("CRON_SPAWNED_AT.store"),
        "run() must stamp its own start (#1925)"
    );
    let stamp = head
        .find("CRON_LAST_TICK_AT.store")
        .expect("tick-completion stamp");
    let ok_arm = head.find("Ok(()) =>").expect("ok arm");
    let err_arm = head.find("Err(e) =>").expect("err arm");
    assert!(
        stamp > ok_arm && stamp < err_arm,
        "the tick stamp must live in the Ok arm: failing ticks are not liveness proof (#1925)"
    );
}

// ── the health seam ─────────────────────────────────────────────────────

#[tokio::test]
async fn health_endpoint_names_the_1925_silence() {
    // Deterministic: the test binary never runs `run()`, so the process-wide
    // stamps stay 0. That is exactly the state of a process whose scheduler
    // never started while its jobs table says jobs are waiting.
    let db = Database::connect_in_memory().await.expect("db");
    db.run_migrations().await.expect("migrations");
    let repo = CronJobRepository::new(db.pool().clone());
    repo.insert(&make_job("liveness-probe"))
        .await
        .expect("insert");

    let app = crate::cli::daemon_health::router(db.pool().clone());
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["cron"]["state"], "not-running-with-jobs");
    assert_eq!(json["cron"]["enabled_jobs"], 1);
}

#[tokio::test]
async fn health_endpoint_idle_without_jobs_or_scheduler() {
    let db = Database::connect_in_memory().await.expect("db");
    db.run_migrations().await.expect("migrations");

    let app = crate::cli::daemon_health::router(db.pool().clone());
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["cron"]["state"], "idle");
    assert_eq!(json["cron"]["enabled_jobs"], 0);
}
