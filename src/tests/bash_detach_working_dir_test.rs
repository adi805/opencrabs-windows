//! Regression (#147 / CRA-8): both detachment routes must run the command in
//! the working directory the caller asked for.
//!
//! `execute` resolves `input.working_dir` (expanding a leading `~`) and
//! validates it, and inline execution uses that resolved path. The two detach
//! paths passed `context.working_directory` instead, so a detached build,
//! write or git operation targeted the session's context directory while the
//! inline route targeted the requested one. The issue describes the failure as
//! "detached builds, writes, or Git operations can therefore target the wrong
//! project"; these tests assert on the write, which is the artifact the user
//! would actually lose.
//!
//! Two distinct directories are used throughout, so a route that ignores the
//! request cannot pass by accident: the marker has to land in `requested` and
//! must be absent from `context_dir`.

use crate::brain::agent::service::MessageEnqueueCallback;
use crate::brain::agent::service::QueuedUserMessage;
use crate::brain::agent::service::background_tasks::BackgroundTaskManager;
use crate::brain::agent::service::session_routes;
use crate::brain::tools::Tool;
use crate::brain::tools::ToolExecutionContext;
use crate::brain::tools::bash::BashTool;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

type DeliveryLog = Arc<Mutex<Vec<(Uuid, QueuedUserMessage)>>>;

/// A route callback that records every completion it receives.
fn recording_route() -> (MessageEnqueueCallback, DeliveryLog) {
    let log: DeliveryLog = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let cb: MessageEnqueueCallback = Arc::new(move |sid, msg| {
        sink.lock().expect("delivery log lock").push((sid, msg));
    });
    (cb, log)
}

/// A throwaway directory that no other test shares.
fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oc-detach-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create test directory");
    dir
}

/// Wait (bounded) for the detached command to report back, then return the
/// completion text.
async fn await_completion(log: &DeliveryLog) -> String {
    let mut waited = 0;
    while log.lock().expect("delivery log lock").is_empty() && waited < 100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }
    let guard = log.lock().expect("delivery log lock");
    assert_eq!(
        guard.len(),
        1,
        "the detached command must report back exactly once"
    );
    guard[0].1.context_text.clone()
}

/// Assert the marker landed in `requested` and nowhere near `context_dir`.
fn assert_marker_landed(requested: &Path, context_dir: &Path, marker: &str) {
    assert!(
        requested.join(marker).exists(),
        "the detached command must run in the requested directory ({})",
        requested.display()
    );
    assert!(
        !context_dir.join(marker).exists(),
        "the detached command must NOT run in the session context directory ({})",
        context_dir.display()
    );
}

fn cleanup(dirs: &[&PathBuf]) {
    for dir in dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The explicit `background: true` route.
#[tokio::test]
// The test_guard serializes suites touching the process-global parked-queue
// state; holding it across the awaits below is the entire point, the same shape
// as the background_tasks and session_notify suites (#1206).
#[allow(clippy::await_holding_lock)]
async fn explicit_background_runs_in_the_requested_directory() {
    // register_session_route touches process-global parked-queue state, so
    // serialize against the other suites that do too (#1206).
    let _guard = crate::brain::agent::service::restart_recovery::test_guard();

    let requested = unique_dir("explicit-requested");
    let context_dir = unique_dir("explicit-context");
    let marker = "detach-marker-explicit.txt";

    let (route, log) = recording_route();
    let sid = Uuid::new_v4();
    session_routes::register_session_route(sid, route);

    let ctx = ToolExecutionContext {
        background_manager: Some(Arc::new(BackgroundTaskManager::new())),
        ..ToolExecutionContext::new(sid)
            .with_auto_approve(true)
            .with_working_directory(context_dir.clone())
    };

    let input = serde_json::json!({
        "command": format!("printf detached > {marker}"),
        "working_dir": requested.to_string_lossy(),
        "background": true,
    });

    let result = BashTool.execute(input, &ctx).await.expect("bash tool call");
    assert!(
        result.success,
        "the detach should be accepted: {}",
        result.output
    );

    await_completion(&log).await;
    assert_marker_landed(&requested, &context_dir, marker);
    cleanup(&[&requested, &context_dir]);
}

/// The automatic route: a command whose first word is a known long marker is
/// detached without the caller asking.
#[tokio::test]
// The test_guard serializes suites touching the process-global parked-queue
// state; holding it across the awaits below is the entire point, the same shape
// as the background_tasks and session_notify suites (#1206).
#[allow(clippy::await_holding_lock)]
async fn automatic_detach_runs_in_the_requested_directory() {
    let _guard = crate::brain::agent::service::restart_recovery::test_guard();

    let requested = unique_dir("auto-requested");
    let context_dir = unique_dir("auto-context");
    let marker = "detach-marker-auto.txt";

    let (route, log) = recording_route();
    let sid = Uuid::new_v4();
    session_routes::register_session_route(sid, route);

    let ctx = ToolExecutionContext {
        background_manager: Some(Arc::new(BackgroundTaskManager::new())),
        ..ToolExecutionContext::new(sid)
            .with_auto_approve(true)
            .with_working_directory(context_dir.clone())
    };

    // `cargo test` is a known long marker, so this detaches on its own. The
    // cargo call itself fails in an empty directory; the marker write after it
    // is what the assertion reads.
    let input = serde_json::json!({
        "command": format!("cargo test; printf auto > {marker}"),
        "working_dir": requested.to_string_lossy(),
    });

    let result = BashTool.execute(input, &ctx).await.expect("bash tool call");
    assert!(
        result.success,
        "the automatic detach should be accepted: {}",
        result.output
    );

    await_completion(&log).await;
    assert_marker_landed(&requested, &context_dir, marker);
    cleanup(&[&requested, &context_dir]);
}
