//! The spawn retry that absorbs a transient kernel refusal.
//!
//! `ETXTBSY` (os 26) is not a corrupt fixture and not a slow spawn: `exec`
//! refuses a file that is open for writing *somewhere*, and a `fork` in another
//! thread inherits every fd the process holds at that instant. A test binary
//! running thousands of tests in parallel therefore keeps a just-written stub
//! counted as busy until the forked child execs, which is what turned
//! `evolve_homebrew_supervision_test` red on a run where 10.670 of 10.671 tests
//! passed.
//!
//! The race itself cannot be pinned from inside a test, so these pin the POLICY
//! apart from it, against synthetic errnos, the way `flock_retry_test` pins
//! `EINTR` apart from a real signal.
#![cfg(unix)]

use crate::brain::tools::evolve::bounded_child::{
    SPAWN_ATTEMPTS, retry_transient_spawn, transient_spawn_error,
};
use std::cell::Cell;
use std::io;

fn errno(n: i32) -> io::Error {
    io::Error::from_raw_os_error(n)
}

#[test]
fn the_busy_file_and_the_unsettled_write_are_transient() {
    assert!(
        transient_spawn_error(&errno(libc::ETXTBSY)),
        "a file another thread's fork still holds open for writing is exactly \
         the case that must be reissued"
    );
    assert!(
        transient_spawn_error(&errno(libc::ENOENT)),
        "an unsettled write is the same class, and the reason health_check_binary \
         retries os 2"
    );
}

#[test]
fn a_real_refusal_is_not_transient() {
    // Retrying these would burn four extra spawns and hide a permission or a
    // format problem behind a generic failure.
    assert!(
        !transient_spawn_error(&errno(libc::EACCES)),
        "a permission problem is an answer, not a delay"
    );
    assert!(
        !transient_spawn_error(&errno(libc::ENOEXEC)),
        "a program that cannot exec will not start exec'ing on the next attempt"
    );
}

#[tokio::test]
async fn a_busy_file_is_reissued_and_the_second_attempt_wins() {
    let calls = Cell::new(0u32);
    let spawned = retry_transient_spawn(|| -> io::Result<&'static str> {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            Err(errno(libc::ETXTBSY))
        } else {
            Ok("spawned")
        }
    })
    .await
    .expect("the second attempt is the one that has to succeed");
    assert_eq!(spawned, "spawned");
    assert_eq!(
        calls.get(),
        2,
        "the busy spawn must be asked again, exactly once"
    );
}

#[tokio::test]
async fn a_clean_spawn_is_not_reissued() {
    let calls = Cell::new(0u32);
    let spawned = retry_transient_spawn(|| -> io::Result<&'static str> {
        calls.set(calls.get() + 1);
        Ok("spawned")
    })
    .await
    .expect("a first-attempt success is returned as-is");
    assert_eq!(spawned, "spawned");
    assert_eq!(calls.get(), 1, "a success must not pay for a retry");
}

#[tokio::test]
async fn a_permanent_error_is_reported_on_the_first_attempt() {
    let calls = Cell::new(0u32);
    let failed = retry_transient_spawn(|| -> io::Result<()> {
        calls.set(calls.get() + 1);
        Err(errno(libc::EACCES))
    })
    .await;
    let e = failed.expect_err("EACCES is a real answer, not a transient");
    assert_eq!(e.raw_os_error(), Some(libc::EACCES));
    assert_eq!(calls.get(), 1, "a real refusal must not be retried");
}

#[tokio::test]
async fn a_refusal_that_never_clears_gives_up_instead_of_spinning() {
    let calls = Cell::new(0u32);
    let failed = retry_transient_spawn(|| -> io::Result<()> {
        calls.set(calls.get() + 1);
        Err(errno(libc::ETXTBSY))
    })
    .await;
    let e = failed.expect_err("a file that stays busy is still a failure");
    assert_eq!(e.raw_os_error(), Some(libc::ETXTBSY));
    assert_eq!(
        calls.get(),
        SPAWN_ATTEMPTS,
        "the cap is what stops the retry, not a lucky attempt"
    );
}
