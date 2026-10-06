//! #1893 regression tests: one unreadable `cron_jobs` row must not silence the
//! scheduler, and a scheduler that keeps failing must stop failing quietly.
//!
//! Two roots, one problem. `list_enabled()` collected `Result<Vec<_>, _>`
//! atomically, so a single row that failed `from_row` returned `Err` and the
//! whole schedule went dark; `tick()` then logged that `Err` and slept 60s
//! forever, with no counter, no escalation and nothing a human would ever see.
//! Field evidence was 5,100 identical tick lines over 3.5 days.

use crate::cron::scheduler::{TICK_ALERT_AFTER_FAILURES, TICK_ALERT_MIN_SPACING, alert_due};
use crate::db::CronJobRepository;
use crate::db::Database;
use crate::db::models::CronJob;
use rusqlite::params;
use std::time::Duration;

/// The scheduler file itself, for the structural sentinels. Same trick
/// `cron_error_chain_test` uses: the loop cannot be driven in a unit test, so
/// the shape of it is pinned here.
const SCHED: &str = include_str!("../cron/scheduler.rs");

/// Extract the body of the item whose signature starts with `head`, from its
/// opening brace to the matching close.
fn enclosing<'a>(src: &'a str, head: &str) -> Option<&'a str> {
    let at = src.find(head)?;
    let open = src[at..].find('{')? + at;
    let mut depth = 0;
    for (i, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[open..open + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

async fn setup() -> (Database, CronJobRepository) {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = CronJobRepository::new(db.pool().clone());
    (db, repo)
}

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

/// Make a row undecodable without touching its `enabled` flag: `id` is read by
/// `uuid_col` first, and a non-UUID `id` fails the decode exactly like the
/// legacy or hand-edited rows the field incident was about. The suffix keeps
/// each corrupted row's `id` distinct: `id` is the primary key, so a fixed
/// literal collides on the second corruption (`SQLITE_CONSTRAINT_UNIQUE`) and
/// the test dies on the fixture instead of on the read.
async fn corrupt_row(db: &Database, name: &str) {
    let name = name.to_string();
    db.pool()
        .get()
        .await
        .expect("get connection")
        .interact(move |conn| {
            conn.execute(
                "UPDATE cron_jobs SET id = 'not-a-uuid-' || ?1 WHERE name = ?1",
                params![name],
            )
        })
        .await
        .expect("interact")
        .expect("corrupt the row");
}

#[tokio::test]
async fn one_unreadable_row_does_not_empty_the_enabled_list() {
    let (db, repo) = setup().await;
    for name in ["job-a", "job-broken", "job-c"] {
        repo.insert(&make_job(name)).await.expect("insert");
    }
    corrupt_row(&db, "job-broken").await;

    // Pre-#1893 this returned Err, which stopped every job, not just the broken
    // one. The two readable rows must survive.
    let jobs = repo.list_enabled().await.expect("read must succeed");
    let names: Vec<String> = jobs.iter().map(|j| j.name.clone()).collect();
    assert_eq!(names, vec!["job-a".to_string(), "job-c".to_string()]);
}

#[tokio::test]
async fn a_clean_table_reports_nothing_skipped() {
    let (_db, repo) = setup().await;
    repo.insert(&make_job("job-a")).await.expect("insert");
    repo.insert(&make_job("job-b")).await.expect("insert");

    let (jobs, skipped) = repo
        .list_enabled_with_skips()
        .await
        .expect("read must succeed");
    assert_eq!(jobs.len(), 2);
    assert_eq!(skipped, 0, "a healthy read must not look degraded");
}

#[tokio::test]
async fn an_all_unreadable_table_is_a_degraded_empty_read() {
    let (db, repo) = setup().await;
    for name in ["job-a", "job-b"] {
        repo.insert(&make_job(name)).await.expect("insert");
    }
    corrupt_row(&db, "job-a").await;
    corrupt_row(&db, "job-b").await;

    // The read itself still succeeds, so "nothing due" and "nothing readable"
    // are only distinguishable through this count. The scheduler turns
    // `jobs.is_empty() && skipped > 0` into a reported failure (#1893).
    let (jobs, skipped) = repo
        .list_enabled_with_skips()
        .await
        .expect("read must succeed");
    assert!(jobs.is_empty());
    assert_eq!(skipped, 2, "both rows must be counted, not swallowed");
}

/// The pacing decision is pure (`alert_due`), so the thresholds the issue asks
/// for are asserted directly instead of by sleeping three minutes in CI (#1893).
#[test]
fn escalation_waits_for_three_consecutive_failures() {
    let hour = Duration::from_secs(3600);
    for failures in 0..TICK_ALERT_AFTER_FAILURES {
        assert!(
            !alert_due(failures, None, hour),
            "failure {failures} of {TICK_ALERT_AFTER_FAILURES} must stay log-only"
        );
    }
    assert!(
        alert_due(TICK_ALERT_AFTER_FAILURES, None, hour),
        "the third consecutive failure must escalate"
    );
    assert!(
        alert_due(TICK_ALERT_AFTER_FAILURES + 7, None, hour),
        "a still-broken scheduler stays eligible to escalate"
    );
}

#[test]
fn escalation_repeats_at_most_hourly() {
    let hour = Duration::from_secs(3600);
    let last = hour * 2;
    assert!(
        !alert_due(9, Some(last), last + Duration::from_secs(120)),
        "a tick failure every 60s must not become 1,440 notices a day"
    );
    assert!(
        !alert_due(
            9,
            Some(last),
            last + TICK_ALERT_MIN_SPACING - Duration::from_millis(1)
        ),
        "the window is closed until it is fully elapsed"
    );
    assert!(
        alert_due(9, Some(last), last + TICK_ALERT_MIN_SPACING),
        "a still-stopped scheduler re-alerts once the window passes"
    );
}

/// A success must clear the counter, otherwise one bad hour after a week of
/// clean ticks escalates a scheduler that is currently fine (#1893).
#[test]
fn a_healthy_tick_clears_the_escalation_state() {
    let src = SCHED;
    let run = enclosing(src, "pub async fn run(self)").expect("run()");
    assert!(
        run.contains("failures = 0"),
        "Ok(()) arm must reset the consecutive-failure counter"
    );
    assert!(
        run.contains("last_alert = None"),
        "a recovery must reset the alert clock too, or the next failure is throttled"
    );
    assert!(
        run.contains("alert_due(failures, last_alert, uptime)"),
        "run() must gate the notice through the pure pacing helper"
    );
    assert!(
        run.contains("alert_tick_failure("),
        "run() must actually send the escalation"
    );
}

/// The tick must read through the resilient path and must not let a total
/// decode failure look like an idle schedule (#1893).
#[test]
fn a_total_read_failure_is_not_reported_as_nothing_due() {
    let src = SCHED;
    let tick = enclosing(src, "async fn tick(&self)").expect("tick()");
    assert!(
        tick.contains("list_enabled_with_skips()"),
        "tick() must read the skipped count, not just the surviving rows"
    );
    assert!(
        tick.contains("jobs.is_empty() && skipped > 0"),
        "\"nothing readable\" must be turned into an Err so the escalation can see it"
    );
    assert!(
        tick.contains("remember_targets(&jobs)"),
        "targets must be cached while the read still works, or a dead scheduler \
         has nowhere to shout"
    );
    assert!(
        !tick.contains("self.repo.list_enabled()"),
        "the atomic read is the defect; the tick must not go back to it"
    );
}

/// The alert string is user-facing and carries the cause chain. It must use the
/// `{:#}` form: a bare `{e}` here would both drop the chain (#1894) and break
/// the chat-discipline test that pins the bare-`{e}` count in this file.
#[test]
fn the_escalation_message_carries_the_cause_chain() {
    let src = SCHED;
    let alert = enclosing(src, "async fn alert_tick_failure(").expect("alert_tick_failure()");
    assert!(
        alert.contains("{err:#}"),
        "the notice has to say why the schedule stopped"
    );
    assert!(
        !alert.contains("{e}"),
        "bare {{e}} is the #1894 defect and would trip its guard test"
    );
    assert!(
        alert.contains("no scheduled job will fire"),
        "the notice must say what is stopped, not just that something failed"
    );
}
