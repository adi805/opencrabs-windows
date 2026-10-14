//! `opencrabs service install` on Linux: a root (sudo) install must produce a
//! *system* unit that runs as the invoking user and starts on boot, while a
//! non-root install produces a plain *user* unit. (The bus-failure handling and
//! honest exit-status reporting live in `run_systemctl`, which shells out to
//! systemctl and so is validated on the box, not here.)
//!
//! Every generated unit must also carry `OOMPolicy=continue`. The daemon is the
//! parent of every tool call and cron child, and systemd's default policy
//! (`stop`) terminates the whole unit when any single child is OOM-killed —
//! turning one tool call's memory spike into a full daemon restart.

use crate::cli::commands::build_systemd_unit;
use crate::tui::onboarding::build_onboarding_systemd_unit;

/// Assert the OOM policy is present, and that it sits on a line of its own.
///
/// `contains` alone is not enough: it would still pass if the directive were
/// buried in a trailing comment, or if the leading indentation leaked into the
/// generated file (systemd requires the key to start the line).
fn assert_oom_policy_continue(unit: &str, what: &str) {
    assert!(
        unit.contains("OOMPolicy=continue"),
        "{what}: generated unit is missing OOMPolicy=continue"
    );
    assert!(
        unit.lines().any(|l| l == "OOMPolicy=continue"),
        "{what}: OOMPolicy=continue is not on a line of its own"
    );
}

#[test]
fn system_unit_runs_as_invoking_user_and_boots() {
    let unit = build_systemd_unit("default", "/home/me/opencrabs daemon", Some("me"), true);
    assert!(unit.contains("User=me"));
    assert!(unit.contains("Group=me"));
    assert!(unit.contains("Environment=HOME=/home/me"));
    assert!(unit.contains("WantedBy=multi-user.target"));
    assert!(unit.contains("ExecStart=/home/me/opencrabs daemon"));
    // Restart=always (not on-failure) so a clean exit also auto-recovers,
    // matching the macOS LaunchAgent's KeepAlive. The daemon exits 0 on a
    // normal shutdown, which on-failure would leave dead.
    assert!(unit.contains("Restart=always"));
    assert_oom_policy_continue(&unit, "system unit");
}

#[test]
fn user_unit_has_no_user_directive_and_targets_default() {
    let unit = build_systemd_unit("default", "/home/me/opencrabs daemon", None, false);
    assert!(!unit.contains("User="));
    assert!(!unit.contains("Environment=HOME="));
    assert!(unit.contains("WantedBy=default.target"));
    assert_oom_policy_continue(&unit, "user unit");
}

#[test]
fn profile_label_is_threaded_into_description() {
    let unit = build_systemd_unit("staging", "/bin/opencrabs -p staging daemon", None, true);
    assert!(unit.contains("Description=OpenCrabs Daemon [staging]"));
}

/// The onboarding wizard writes its own hand-rolled unit rather than calling
/// `build_systemd_unit`, so it needs its own coverage: without the directive a
/// fresh wizard install keeps the dangerous default. Reached through the
/// `cfg(test)` bridge in `crate::tui::onboarding`.
#[test]
fn onboarding_unit_survives_child_oom_kill() {
    let unit = build_onboarding_systemd_unit("/usr/local/bin/opencrabs");
    assert_oom_policy_continue(&unit, "onboarding unit");
    assert!(unit.contains("ExecStart=/usr/local/bin/opencrabs daemon"));
    assert!(unit.contains("WantedBy=default.target"));
}

/// The pre-write guard the wizard consults before it writes a user unit.
///
/// On a box that already has a system unit for the same daemon, writing a
/// second one leaves both enabled and both fighting over the profile instance
/// lock: the loser exits on every start, and a supervisor that restarts it
/// loops until its budget is gone, which is how the bot stayed down.
mod system_unit_guard {
    use crate::tui::onboarding::system_unit_for_profile;

    #[test]
    fn finds_the_canonical_system_unit() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join("opencrabs.service");
        std::fs::write(&unit, "[Unit]\n").unwrap();

        assert_eq!(system_unit_for_profile(dir.path()), Some(unit));
    }

    /// A hand-tuned unit under a different name is still the same daemon. This
    /// is the shape seen in the field (`opencrabs-tui.service`), and exactly
    /// what a bare `exists("opencrabs.service")` check would miss.
    #[test]
    fn finds_a_differently_named_daemon_unit() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join("opencrabs-tui.service");
        std::fs::write(&unit, "[Unit]\n").unwrap();

        assert_eq!(system_unit_for_profile(dir.path()), Some(unit));
    }

    /// The canonical name wins when both are present, so the refusal names the
    /// most obvious unit rather than an arbitrary one.
    #[test]
    fn prefers_the_canonical_name() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("opencrabs.service");
        std::fs::write(&canonical, "[Unit]\n").unwrap();
        std::fs::write(dir.path().join("opencrabs-tui.service"), "[Unit]\n").unwrap();

        assert_eq!(system_unit_for_profile(dir.path()), Some(canonical));
    }

    /// No daemon unit means no refusal: unrelated units and stray files must
    /// not trip the guard.
    #[test]
    fn ignores_unrelated_units() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("caddy.service"), "[Unit]\n").unwrap();
        std::fs::write(dir.path().join("opencrabs-notes.txt"), "x").unwrap();

        assert_eq!(system_unit_for_profile(dir.path()), None);
    }

    /// A missing directory is not an existing unit.
    #[test]
    fn returns_none_for_empty_and_missing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(system_unit_for_profile(dir.path()), None);
        assert_eq!(system_unit_for_profile(&dir.path().join("nope")), None);
    }
}
