//! Dropped tool futures must stop their child process (#1046).
//!
//! A timed-out tool drops its future, which drops the `tokio::process::Child`.
//! Tokio does NOT kill the process on drop unless `kill_on_drop(true)` was set,
//! so without it a timed-out `grep` kept running after its turn had settled and
//! reported a final answer produced without it.
//!
//! These pin the semantics the fix relies on. If a tokio upgrade ever changed
//! them, the tools would silently start leaking processes again and nothing
//! else in the suite would notice.
//!
//! Liveness is read from `/proc/<pid>/stat`, NOT from `kill -0`. `kill -0`
//! answers "does this pid exist", which a zombie still does: it cannot tell a
//! running process from one that has already exited and is merely awaiting
//! collection. Probing that way produced false failures under coverage — both
//! tests pass on the plain suite, and under tarpaulin `kill -0` still reported
//! the pid 3s after the drop, while the process was already gone. The guarantee
//! worth pinning is that the command stops RUNNING, so a zombie (`Z`) or a
//! stopped process (`T`/`t`) counts as stopped; the state character also makes
//! a genuine failure legible, since a runnable state means the kill itself
//! never landed rather than that the reap is late.

use tokio::process::Command;

/// Read the state character from `/proc/<pid>/stat`, or `None` when the
/// process no longer exists. The `comm` field can contain spaces and
/// parentheses, so the state is the first character after the FINAL `)`.
#[cfg(target_os = "linux")]
fn process_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rfind(')').map(|i| &stat[i + 1..])?;
    after_comm.trim_start().chars().next()
}

/// No `/proc` off Linux: fall back to `kill -0`, which only distinguishes
/// existence. The tests that need the finer distinction run on Linux CI.
#[cfg(not(target_os = "linux"))]
fn process_state(pid: u32) -> Option<char> {
    let exists = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    exists.then_some('R')
}

/// Can the process still execute? `Z` (exited, awaiting reap), `X` (dead) and
/// `T`/`t` (stopped) all mean it is not running any more.
fn is_running(pid: u32) -> bool {
    matches!(process_state(pid), Some('R' | 'S' | 'D' | 'I'))
}

/// Poll until the process stops running, returning the last state it was seen
/// in while still runnable — so a failure says why, not just that it failed.
async fn still_running_after(pid: u32, tries: u32) -> Option<char> {
    let mut last = None;
    for _ in 0..tries {
        if !is_running(pid) {
            return None;
        }
        last = process_state(pid);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    last
}

#[tokio::test]
async fn dropping_a_child_with_kill_on_drop_stops_the_process() {
    let child = Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep");
    let pid = child.id().expect("child has a pid");
    assert!(is_running(pid), "sanity: the process started");

    drop(child);

    let state = still_running_after(pid, 30).await;
    assert!(
        state.is_none(),
        "kill_on_drop(true) must stop the child when the future is dropped, \
         but it was still running (state {state:?}) 3s later"
    );
}

#[tokio::test]
async fn a_timed_out_command_does_not_outlive_its_future() {
    // The reported shape: a long command wrapped in a timeout. When the
    // timeout fires the future is dropped, and the process must go with it.
    let mut child = Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep");
    let pid = child.id().expect("child has a pid");

    let waited = tokio::time::timeout(std::time::Duration::from_millis(200), child.wait()).await;
    assert!(waited.is_err(), "sanity: the command outlives its timeout");

    drop(child);

    let state = still_running_after(pid, 30).await;
    assert!(
        state.is_none(),
        "a timed-out command must not keep running after its turn moves on, \
         but it was still running (state {state:?}) 3s later"
    );
}
