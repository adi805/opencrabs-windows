#![cfg(windows)]
//! Tests for the Windows advisory lock and the birth-checked terminate
//! (`crate::config::winlock`).
//!
//! Extracted from an inline `#[cfg(test)]` block in `src/config/winlock.rs`;
//! project policy (CONTRIBUTING.md, "Where Tests Live") requires every test to
//! live under `src/tests/`, and the repo has been moving them there, not the
//! other way: 14 files here carry the same "extracted from an inline block"
//! note. Windows-gated because the module it tests is: range locks and
//! `GetProcessTimes` have no Unix counterpart in this tree.
//!
//! These run in the Windows slice of `ci.yml`, not the Linux test job, which is
//! the whole reason the slice exists.

use std::os::windows::io::AsRawHandle;

use crate::config::winlock::{
    LockOutcome, creation_ticks_of, exclusive, own_creation_ticks, unlock,
};

/// The stamp's creation field is what makes the birth proof exact, so it
/// must be present on Windows: a `None` here would make every stamp
/// unverifiable and the handover refuse -- fail-closed, but useless. The
/// value must also be STABLE, since `terminate` compares it for equality.
#[test]
fn own_creation_ticks_is_present_and_stable() {
    let a = own_creation_ticks().expect("GetProcessTimes must work on Windows");
    let b = own_creation_ticks().expect("GetProcessTimes must work on Windows");
    assert_eq!(a, b, "a process's creation time must not change");
    assert!(a > 0, "a creation time of zero means the query lied");
}

/// The reader the lock scan uses to tell a live owner from a stale stamp
/// must return, for a process, the SAME number that process reads for
/// itself -- the two sides of `terminate`'s equality check. It must also
/// refuse rather than guess when the handle cannot be taken.
#[test]
fn creation_ticks_of_matches_the_own_reading_and_refuses_a_bogus_pid() {
    let me = std::process::id();
    assert_eq!(
        creation_ticks_of(me),
        own_creation_ticks(),
        "the reader and the writer must share one clock"
    );
    assert_eq!(
        creation_ticks_of(u32::MAX),
        None,
        "an invalid PID has no creation time to report"
    );
}

/// Range-lock contention is per-HANDLE, so a second handle to the same
/// file in the SAME process contends exactly like a second process.
/// That makes the Held/Acquired/unlock cycle testable without spawning
/// anything.
#[test]
fn non_blocking_second_handle_reports_held() {
    let dir = std::env::temp_dir().join(format!("winlock-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.lock");
    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap()
    };
    let a = open();
    let b = open();
    // Format the outcome rather than asserting `matches!` alone: a
    // `Failed` carries the Win32 error, and without it a regression here
    // reports only "not Acquired" and costs a CI round-trip to name the
    // actual failure.
    let first = exclusive(a.as_raw_handle(), true);
    assert!(
        matches!(first, LockOutcome::Acquired),
        "the first handle must take the sentinel range, got {first:?}"
    );
    let second = exclusive(b.as_raw_handle(), true);
    assert!(
        matches!(second, LockOutcome::Held),
        "a second handle must see the range held, got {second:?}"
    );
    assert!(unlock(a.as_raw_handle()).is_ok());
    let after_unlock = exclusive(b.as_raw_handle(), true);
    assert!(
        matches!(after_unlock, LockOutcome::Acquired),
        "the range must be free once the holder unlocks, got {after_unlock:?}"
    );
    // Windows cannot remove a file while a handle is open on it, and
    // cannot remove a non-empty dir either: drop every handle first,
    // then assert the cleanup itself. Ignoring this result let the
    // temp dir leak on every run without anyone noticing.
    drop(a);
    drop(b);
    std::fs::remove_dir_all(&dir).expect("test temp dir cleanup");
}
