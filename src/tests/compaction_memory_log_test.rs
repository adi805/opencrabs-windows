//! The daily memory log is appended to, and the append is a read-modify-write
//! (F-BR-02).
//!
//! Two compactions that overlap both read `existing` and both write, so the
//! later write wins over a copy that never saw the earlier one and one summary
//! is lost with no error anywhere. Background compaction makes the overlap
//! possible rather than theoretical, and the loss is silent: the day's log
//! looks complete because every file it holds is well formed.
//!
//! The write also used `std::fs::write`, which opens with `O_TRUNC`, so between
//! the truncate and the write the log is zero bytes on disk and a process that
//! dies mid-write leaves it that way.
//!
//! These tests drive the append against a temp path, on the current-thread
//! runtime `#[tokio::test]` gives by default. The read is synchronous and sits
//! before the first `.await`, so without the lock every task reaches its read
//! before any task's rename lands: the loss this covers reproduces there instead
//! of interleaving at random.

use crate::brain::agent::service::context::append_compaction_summary;
use std::path::Path;

const HEADER: &str = "## Auto-Compaction Summary";

fn stamp(minute: u32) -> chrono::DateTime<chrono::Local> {
    use chrono::TimeZone;
    chrono::Local
        .with_ymd_and_hms(2026, 10, 16, 9, minute, 0)
        .single()
        .expect("a real local time")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[tokio::test]
async fn a_summary_lands_in_an_empty_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("2026-10-16.md");

    append_compaction_summary(&log, "first body", stamp(5))
        .await
        .expect("append");

    let body = read(&log);
    // The writer emits `{existing}\n\n---\n\n{header}...`, so an empty log opens
    // with the rule and the header follows it. Asserting the header alone is
    // wrong and was caught here: the file starts with `---`, not the header.
    assert!(
        body.starts_with(&format!("---\n\n{HEADER}")),
        "the log must open with the rule and then the header: {body:?}"
    );
    assert!(
        body.contains("(2026-10-16 09:05:00)"),
        "the stamp must be local time: {body}"
    );
    assert!(body.contains("first body"), "{body}");
}

/// The on-disk shape is user-visible (it is a file they read), so the append has
/// to keep the format the truncating write produced: the previous entry, a rule,
/// then the new one, with the trailing blank lines normalized away.
#[tokio::test]
async fn a_summary_appends_below_an_existing_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("2026-10-16.md");
    std::fs::write(&log, "user's own note\n\n\n").expect("seed the log");

    append_compaction_summary(&log, "second body", stamp(7))
        .await
        .expect("append");

    let body = read(&log);
    assert!(
        body.starts_with("user's own note\n\n---\n\n"),
        "the existing entry survives, the trailing blanks collapse: {body:?}"
    );
    assert!(body.contains("second body"), "{body}");
    assert_eq!(
        body.matches(HEADER).count(),
        1,
        "one append, one header: {body}"
    );
}

/// The defect: an append that reads a copy the previous append has not published
/// yet overwrites it. Eight overlapping appends must leave eight summaries.
///
/// The mechanism is what makes this fail rather than flake. The read is
/// synchronous and sits before the first `.await`, so on the current-thread
/// runtime every task reaches its read before any task's rename lands, and the
/// writes then overwrite each other in turn instead of interleaving at random.
/// The serializing lock covers the read and the write together, which is the
/// pair the defect lives in.
#[tokio::test]
async fn overlapping_appends_lose_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("2026-10-16.md");

    const N: u32 = 8;
    let futures = (0..N).map(|i| {
        let log = log.clone();
        async move { append_compaction_summary(&log, &format!("body number {i}"), stamp(i)).await }
    });
    let results = futures::future::join_all(futures).await;
    for (i, result) in results.iter().enumerate() {
        assert!(result.is_ok(), "append {i} failed: {result:?}");
    }

    let body = read(&log);
    assert_eq!(
        body.matches(HEADER).count(),
        N as usize,
        "overlapping appends lost a summary: {body}"
    );
    for i in 0..N {
        assert!(
            body.contains(&format!("body number {i}")),
            "summary {i} is missing from the log: {body}"
        );
    }
}

/// The publish goes through a sibling temp file. One stranded beside the log
/// would sit in the user's memory directory forever, since nothing sweeps it.
#[tokio::test]
async fn no_temp_file_is_left_beside_the_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("2026-10-16.md");

    append_compaction_summary(&log, "only body", stamp(3))
        .await
        .expect("append");

    let names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["2026-10-16.md".to_string()],
        "the memory directory must hold the log and nothing else"
    );
}

/// A log that cannot be published must not be left half-written. The old
/// content stays readable and the caller learns the append failed.
#[tokio::test]
async fn a_refused_publish_reports_and_leaves_the_log_readable() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A directory in the log's place: the temp file writes fine, the rename
    // cannot land on a non-empty directory.
    let log = dir.path().join("2026-10-16.md");
    std::fs::create_dir(&log).expect("create the blocker");
    std::fs::write(log.join("occupant"), "keeps the directory non-empty").expect("occupant");

    let err = append_compaction_summary(&log, "never lands", stamp(9))
        .await
        .expect_err("a rename onto a non-empty directory cannot succeed");

    assert!(
        err.contains("Failed to write daily memory log"),
        "the caller must be able to say what failed: {err}"
    );
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["2026-10-16.md".to_string()],
        "the failed publish stranded a temp file: {names:?}"
    );
}
