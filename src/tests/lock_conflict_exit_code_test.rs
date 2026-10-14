//! The lock-conflict refusal must be distinguishable from a generic failure.
//!
//! `acquire_profile_instance` refuses to start a second headless instance of a
//! profile whose lock is already held, and exits. That exit used to be `1`,
//! which is what a crash, a bad config, and a missing key all produce — so a
//! supervisor could not tell "another instance is running, leave it alone" from
//! "the daemon died, restart it". On a box with both a system and a user unit
//! enabled for the same profile, a watchdog restarted the loser every few
//! seconds until its hourly budget was gone, then stopped watching, and the bot
//! stayed down. `EXIT_LOCK_CONFLICT` (75, `EX_TEMPFAIL`) is the signal that lets
//! a supervisor choose correctly instead of guessing.

use crate::cli::ui::EXIT_LOCK_CONFLICT;

/// The value is part of the contract with supervisors: `sysexits.h` defines 75
/// as `EX_TEMPFAIL` (transient, retry later). Changing it silently would break
/// every watchdog that learns to read it.
#[test]
fn lock_conflict_exit_code_is_ex_tempfail() {
    assert_eq!(EXIT_LOCK_CONFLICT, 75);
}

/// A regression guard on the source text itself.
///
/// The two exit calls cannot be reached from a test: they sit behind a real
/// flock held by another live process, and reaching them would mean spawning a
/// competing instance. So the guard anchors on the file instead, which is
/// enough to catch the actual regression mode — someone adding a new refusal
/// path with a bare `exit(1)`, or reverting one of the two.
#[test]
fn refusal_paths_do_not_use_a_bare_exit_one() {
    let src = include_str!("../cli/ui.rs");

    assert_eq!(
        src.matches("std::process::exit(1)").count(),
        0,
        "ui.rs still contains a bare exit(1); a lock conflict would be \
         indistinguishable from a crash and a supervisor would restart the loser"
    );

    // Two sites carry it: the immediate refusal, and the refusal after a failed
    // preemption. Both must use the distinct code.
    assert_eq!(
        src.matches("std::process::exit(EXIT_LOCK_CONFLICT)")
            .count(),
        2,
        "expected exactly two lock-conflict exit sites in ui.rs"
    );
}
