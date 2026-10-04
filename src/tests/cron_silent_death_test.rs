//! #1893 regression tests: one unreadable `cron_jobs` row must not silence the
//! scheduler, and a scheduler that keeps failing must stop failing quietly.
//!
//! Two roots, one problem. `list_enabled()` collected `Result<Vec<_>, _>`
//! atomically, so a single row that failed `from_row` returned `Err` and the
//! whole schedule went dark; `tick()` then logged that `Err` and slept 60s
//! forever, with no counter, no escalation and nothing a human would ever see.
//! Field evidence was 5,100 identical tick lines over 3.5 days.

use crate::db::CronJobRepository;
use crate::db::Database;
use crate::db::models::CronJob;
use rusqlite::params;

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
