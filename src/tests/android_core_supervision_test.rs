//! The Android service must supervise the core it spawns.
//!
//! `CoreService` starts the Rust core as a child process and drains its stdout.
//! The original version treated that drain as a logging loop: when the pipe hit
//! EOF, which is the only exit notification the process gets for free, it logged
//! "output pump ended" and returned. Nothing observed the death, the ongoing
//! notification kept advertising "running", and no attempt was made to bring the
//! core back. `onDestroy` called `destroy()` and never confirmed the child left.
//!
//! The app is Java, which nothing in this repo compiles before merge: the only
//! job running `javac` is `android-artifact.yml`. So these tests read the Java
//! source, which runs on every PR, and pin the properties the supervision
//! argument rests on: EOF is an exit signal, the restart is bounded, a stop the
//! operator asked for is not undone, and the notification tells the truth.

/// The service that spawns and supervises the core process.
const CORE_SERVICE: &str = "android/java/io/opencrabs/mobile/CoreService.java";

fn core_service() -> String {
    let path = format!("{}/{CORE_SERVICE}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// The body of a method, from its signature line to its closing brace at
/// method indent. Nested blocks are indented deeper, so the first `\n    }`
/// after the signature is the method's own.
fn method_body<'a>(src: &'a str, signature: &str) -> &'a str {
    let start = src.find(signature).unwrap_or_else(|| {
        panic!("CoreService no longer declares `{signature}`; update this test")
    });
    let rest = &src[start..];
    let end = rest
        .find("\n    }")
        .unwrap_or_else(|| panic!("body of `{signature}` not found; update this test"));
    &rest[..end]
}

/// Java source with comment lines removed, so a `contains` check cannot be
/// satisfied by the prose that documents the very trap it is guarding.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with('*') && !t.starts_with("//") && !t.starts_with("/*")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_output_pump_treats_eof_as_an_exit_signal() {
    let src = core_service();
    let drain = method_body(&src, "private void drain(");
    assert!(
        drain.contains("scheduleRestart()"),
        "the stdout pump no longer reacts to EOF. A read returning -1 means the \
         core is dead, and logging that and returning leaves a corpse behind a \
         notification that still says running, with nothing to bring it back."
    );
    // The stop/newer-spawn guard has to be checked before the restart, or a
    // deliberate shutdown turns into a respawn loop.
    let guard = drain.find("if (stopping || spawned != core)").expect(
        "drain no longer distinguishes an expected exit (an operator stop, or a \
             newer core already owning the field) from a crash. Without it every \
             onDestroy is answered with a restart.",
    );
    let restart = drain.find("scheduleRestart()").unwrap();
    assert!(
        guard < restart,
        "drain restarts before checking whether the exit was expected"
    );
}

#[test]
fn a_restart_is_bounded_and_the_backoff_is_capped() {
    let src = core_service();
    let restart = method_body(&src, "private void scheduleRestart()");
    assert!(
        restart.contains("if (stopping)"),
        "scheduleRestart no longer bails out when the service is stopping"
    );
    assert!(
        restart.contains("restarts >= MAX_RESTARTS"),
        "the restart count is no longer bounded. An unbounded respawn turns a \
         config error into a crash loop that runs until the battery is flat."
    );
    assert!(
        restart.contains("Thread.sleep(delay)"),
        "the restart is no longer delayed, so a core that dies instantly spins"
    );
    let backoff = method_body(&src, "static long backoffMs(");
    assert!(
        backoff.contains("return MAX_BACKOFF_MS"),
        "the backoff is no longer capped; repeated doubling eventually sleeps for \
         longer than any operator will wait for a retry"
    );
}

#[test]
fn an_operator_stop_is_not_undone() {
    let src = core_service();
    let destroy = method_body(&src, "public void onDestroy()");
    let flag = destroy.find("stopping = true").expect(
        "onDestroy no longer marks the service as stopping, so the pump thread \
         cannot tell a shutdown from a crash and respawns the core it just killed",
    );
    let kill = destroy.find(".destroy()").expect(
        "onDestroy no longer destroys the core process; the child outlives the \
         service that owns it",
    );
    assert!(
        flag < kill,
        "onDestroy sets the stopping flag after destroy(). The pump thread reads \
         that flag when the pipe closes, so the ordering is what makes a \
         deliberate stop final."
    );
    assert!(
        destroy.contains("reapExitCode("),
        "onDestroy no longer confirms the child exited. destroy() only asks; \
         nothing verifies the core actually left."
    );
}

#[test]
fn a_sticky_restart_clears_a_previous_stop() {
    let src = core_service();
    let start = method_body(&src, "public int onStartCommand(");
    let clear = start.find("stopping = false").expect(
        "onStartCommand no longer clears the stopping flag. The platform restarts \
         a START_STICKY service with a fresh instance whose field default is \
         already false, but the same instance can also be handed a new intent \
         after onDestroy, and then it would come up believing it is shutting \
         down and never spawn the core.",
    );
    let spawn = start
        .find("spawnCore()")
        .expect("onStartCommand no longer spawns the core; update this test");
    assert!(
        clear < spawn,
        "onStartCommand spawns before clearing the stop flag"
    );
}

#[test]
fn the_notification_reports_the_actual_state() {
    let src = core_service();
    let code = code_only(&src);
    assert!(
        !code.contains(".setContentText(\"running\")"),
        "the notification text is hardcoded to \"running\" again. The notification \
         is the only signal an operator has without logcat, so a dead core must \
         not be advertised as running."
    );
    assert!(
        code.contains("private void setState("),
        "the service no longer republishes its notification state"
    );
    let drain = method_body(&src, "private void drain(");
    let restart = method_body(&src, "private void scheduleRestart()");
    assert!(
        restart.contains("setState("),
        "the restart path no longer updates the notification, so an operator \
         cannot tell a restarting core from a running one"
    );
    assert!(
        drain.contains("scheduleRestart()"),
        "the crash path no longer reaches the code that republishes state"
    );
}

#[test]
fn the_child_is_reaped_without_api_26_only_calls() {
    let src = core_service();
    let code = code_only(&src);
    assert!(
        code.contains("IllegalThreadStateException"),
        "the exit-code probe no longer handles a still-running child; \
         exitValue() throws rather than returning -1, and an uncaught throw on \
         the pump thread silently ends supervision"
    );
    assert!(
        !code.contains("waitFor("),
        "the supervisor now calls waitFor(). Process.waitFor(long, TimeUnit) is \
         API 26 and this file has already been bitten once by a java.lang method \
         that javac resolved from the JDK and the device did not have."
    );
}
