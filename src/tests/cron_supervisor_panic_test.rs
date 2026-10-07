//! #1963 regression: a panicking cron tick must surface through the
//! supervisor as a logged error instead of silently killing the scheduler
//! task (the Oct 2026 incident: the delivery truncation panic took the whole
//! TUI process with it, and with a discarded JoinHandle nobody could even
//! see the schedule die).

use crate::cron::scheduler::supervise_tick_result;

/// A real panicking task gives a real `JoinError::Panic` (no mocks: real
/// runtime, real panic, real join), and the supervisor must convert it to an
/// ordinary error carrying both markers the operator needs.
#[tokio::test]
async fn panicking_tick_maps_to_supervisor_error() {
    let handle: tokio::task::JoinHandle<Result<(), anyhow::Error>> =
        tokio::spawn(async { panic!("simulated cron tick panic") });
    let joined = handle.await;
    assert!(joined.is_err(), "panic must surface as JoinError");

    let err = supervise_tick_result(joined).expect_err("panic maps to Err");
    let msg = err.to_string();
    assert!(msg.contains("panicked"), "msg was: {msg}");
    assert!(msg.contains("scheduler keeps running"), "msg was: {msg}");
    assert!(msg.contains("simulated cron tick panic"), "msg was: {msg}");
}

#[tokio::test]
async fn cancelled_tick_maps_to_cancelled_error() {
    let handle: tokio::task::JoinHandle<Result<(), anyhow::Error>> =
        tokio::spawn(async { std::future::pending::<Result<(), anyhow::Error>>().await });
    handle.abort();
    let err = supervise_tick_result(handle.await).expect_err("cancel maps to Err");
    let msg = err.to_string();
    assert!(msg.contains("cancelled"), "msg was: {msg}");
    assert!(msg.contains("scheduler keeps running"), "msg was: {msg}");
}

#[tokio::test]
async fn ok_and_failed_ticks_pass_through_unchanged() {
    assert!(supervise_tick_result(Ok(Ok(()))).is_ok());
    let e = supervise_tick_result(Ok(Err(anyhow::anyhow!("db down"))))
        .expect_err("inner error must pass through");
    assert_eq!(e.to_string(), "db down");
}
