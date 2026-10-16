//! Regression: a broken `config.toml` must NOT poison the last-known-good
//! snapshot, and `Config::load()` must recover from it.
//!
//! The bug: the config watcher called `Config::load()` (which already recovers
//! from last-good, so it returns Ok even when config.toml is broken) and then
//! `save_last_good_config()` did a RAW `fs::copy` of config.toml over the good
//! snapshot. A single malformed edit (an unterminated `models = [...]` array)
//! therefore corrupted BOTH config.toml and its snapshot at once. The next
//! `Config::load()` had no valid last-good to fall back to, returned an error,
//! and the approval check defaulted to "ask" — flipping auto-always (yolo)
//! users into tool-approval prompts after months without one.
//!
//! Fix: `save_last_good_config()` refuses to snapshot a config that does not
//! parse, and the watcher skips the snapshot when the load was a recovery.
//!
//! These tests scope the config home with `with_home_override` rather than
//! pointing `$HOME` at a tempdir. `$HOME` is process-global: any test loading
//! config in parallel resolved into this tempdir and rewrote the deliberately
//! broken config.toml, which is what made the suite order-dependent (#912).

use crate::config::profile::with_home_override;
use crate::config::{Config, load_last_good_config, opencrabs_home, save_last_good_config};

const GOOD_CONFIG: &str = r#"
[agent]
approval_policy = "auto-always"
"#;

// Mirrors the real failure: an unterminated array breaks the whole TOML file.
// This one IS mechanically repairable (just a missing `]`).
const FIXABLE_BROKEN: &str = r#"
[agent]
approval_policy = "auto-always"

[providers.custom.broken]
models = ["a", "b", "c"
"#;

// A non-delimiter syntax error the repairer can't safely fix — must fall back
// to last-known-good.
const UNFIXABLE_BROKEN: &str = r#"
[agent]
approval_policy = "auto-always" / oops
"#;

/// A tempdir laid out as an `.opencrabs` home, plus the config it starts with.
/// Returns the tempdir so the caller keeps it alive for the whole test.
fn temp_home_with(config_toml: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let opencrabs = dir.path().join(".opencrabs");
    std::fs::create_dir_all(&opencrabs).expect("create .opencrabs");
    std::fs::write(opencrabs.join("config.toml"), config_toml).expect("write config");
    std::fs::write(opencrabs.join("keys.toml"), b"").expect("write keys");
    (dir, opencrabs)
}

#[test]
fn broken_config_does_not_poison_last_good_snapshot() {
    let (_temp, home) = temp_home_with(GOOD_CONFIG);
    with_home_override(home, || {
        // Snapshot the good config — last-good now holds auto-always.
        save_last_good_config();
        let good = load_last_good_config().expect("snapshot of a valid config must load");
        assert_eq!(good.agent.approval_policy, "auto-always");

        // The user saves a malformed config.toml. Snapshotting again MUST refuse —
        // the broken file must not overwrite the good snapshot (the regression that
        // poisoned recovery and flipped yolo mode into approval prompts).
        let config_path = opencrabs_home().join("config.toml");
        std::fs::write(&config_path, UNFIXABLE_BROKEN).expect("overwrite with broken config");
        save_last_good_config();
        let still_good =
            load_last_good_config().expect("snapshot must still be the prior VALID config");
        assert_eq!(
            still_good.agent.approval_policy, "auto-always",
            "a broken config.toml must NOT clobber the last-good snapshot"
        );
    });
}

#[test]
fn fixable_broken_config_is_auto_repaired_in_place() {
    let (_temp, home) = temp_home_with(FIXABLE_BROKEN);
    with_home_override(home, || {
        let config_path = opencrabs_home().join("config.toml");

        // Precondition: the file does not parse as-is.
        let raw = std::fs::read_to_string(&config_path).unwrap();
        assert!(toml::from_str::<toml::Value>(&raw).is_err());

        // load() repairs the missing `]`, saves it back, and loads cleanly —
        // preserving auto-always so yolo mode survives a typo.
        let (cfg, status) = Config::load_with_status().expect("load must auto-repair and succeed");
        assert_eq!(cfg.agent.approval_policy, "auto-always");
        assert!(
            status.autofixed,
            "load should report it auto-repaired the file"
        );

        // The fix is persisted: config.toml now parses, and the broken original was
        // backed up.
        let healed = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            toml::from_str::<toml::Value>(&healed).is_ok(),
            "config.toml must be valid on disk after auto-repair"
        );
        assert!(
            config_path.with_extension("toml.autofix.bak").exists(),
            "the broken original must be backed up"
        );
    });
}

#[test]
fn unfixable_broken_config_recovers_from_last_good() {
    let (_temp, home) = temp_home_with(GOOD_CONFIG);
    with_home_override(home, || {
        // Establish a good snapshot, then break config.toml unfixably.
        save_last_good_config();
        let config_path = opencrabs_home().join("config.toml");
        std::fs::write(&config_path, UNFIXABLE_BROKEN).expect("overwrite with unfixable config");

        // load() can't repair it, so it falls back to last-known-good — preserving
        // auto-always rather than defaulting to an approval prompt.
        let (recovered, status) =
            Config::load_with_status().expect("load must recover from last-good, not error out");
        assert_eq!(
            recovered.agent.approval_policy, "auto-always",
            "recovery must preserve the auto-always policy so yolo mode survives a broken edit"
        );
        assert!(
            status.recovered,
            "the load should report it fell back to last-known-good"
        );
        assert!(
            status.recovery_reason.is_some(),
            "recovery must carry the parse error that caused it (#909)"
        );
    });
}

// ── a stale snapshot recovers, and the age is reported (F-CFG-04) ─────

/// Recovery from an OLD snapshot is deliberate, and this pins it so nobody
/// "fixes" the message by turning the fallback into a refusal.
///
/// The alternative is refusing to start on a config the user cannot fix without
/// the daemon running, and a working daemon on older settings beats no daemon.
/// What the user needs is the age of the settings in effect, which is why it
/// travels on the status rather than sitting only in the log.
#[test]
fn a_stale_snapshot_still_recovers_and_reports_its_age() {
    let (_temp, home) = temp_home_with(GOOD_CONFIG);
    with_home_override(home, || {
        save_last_good_config();

        // Backdate the snapshot by 31 days. `File::set_modified` rather than a
        // sleep, so this cannot flake on a filesystem clock.
        let snapshot = crate::config::last_good_config_path();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&snapshot)
            .expect("open the snapshot")
            .set_modified(old)
            .expect("backdate the snapshot");

        let age = crate::config::last_good_snapshot_age().expect("the snapshot has an mtime");
        assert!(
            age.as_secs() >= 31 * 86_400,
            "the backdate must be visible to the age reader: {age:?}"
        );

        // Break config.toml unfixably so the load has to recover.
        let config_path = opencrabs_home().join("config.toml");
        std::fs::write(&config_path, UNFIXABLE_BROKEN).expect("overwrite with unfixable config");

        let (recovered, status) = Config::load_with_status()
            .expect("a stale snapshot still recovers, it does not refuse");
        assert_eq!(
            recovered.agent.approval_policy, "auto-always",
            "the stale snapshot's values are what the daemon runs on"
        );
        assert!(status.recovered, "the load reports the recovery");
        assert!(
            status.recovery_reason.is_some(),
            "and the cause, so the user knows what to fix (#909)"
        );
        let secs = status
            .snapshot_age_secs
            .expect("the age must travel with the status (F-CFG-04)");
        assert!(
            secs >= 31 * 86_400,
            "the reported age must be the snapshot's, not a fresh one: {secs}"
        );
        assert!(
            crate::config::humanize_age(std::time::Duration::from_secs(secs)).contains("d "),
            "31 days has to render as days, not as 744h: {}",
            crate::config::humanize_age(std::time::Duration::from_secs(secs))
        );
    });
}

/// A clean load carries no age, so the notice cannot claim staleness when there
/// was no recovery.
#[test]
fn a_clean_load_reports_no_snapshot_age() {
    let (_temp, home) = temp_home_with(GOOD_CONFIG);
    with_home_override(home, || {
        let (_cfg, status) = Config::load_with_status().expect("a valid config loads");
        assert!(!status.recovered);
        assert!(
            status.snapshot_age_secs.is_none(),
            "a clean load has no snapshot in effect, so no age"
        );
    });
}

/// The age renderer, on the three bands an operator reads.
#[test]
fn ages_render_in_the_band_they_belong_to() {
    use crate::config::humanize_age;
    use std::time::Duration;

    assert_eq!(humanize_age(Duration::from_secs(0)), "0m");
    assert_eq!(humanize_age(Duration::from_secs(59)), "0m");
    assert_eq!(humanize_age(Duration::from_secs(60)), "1m");
    assert_eq!(humanize_age(Duration::from_secs(3_599)), "59m");
    assert_eq!(humanize_age(Duration::from_secs(3_600)), "1h 0m");
    assert_eq!(humanize_age(Duration::from_secs(3_600 + 60)), "1h 1m");
    assert_eq!(humanize_age(Duration::from_secs(86_399)), "23h 59m");
    assert_eq!(humanize_age(Duration::from_secs(86_400)), "1d 0h");
    assert_eq!(humanize_age(Duration::from_secs(31 * 86_400)), "31d 0h");
    assert_eq!(
        humanize_age(Duration::from_secs(31 * 86_400 + 3_600)),
        "31d 1h"
    );
}

/// The snapshot path is one definition, and the age reader reports nothing when
/// there is no snapshot rather than inventing an age.
#[test]
fn the_age_reader_needs_a_snapshot() {
    let (_temp, home) = temp_home_with(GOOD_CONFIG);
    with_home_override(home, || {
        assert!(
            !crate::config::last_good_config_path().exists(),
            "fixture: no snapshot has been written yet"
        );
        assert!(
            crate::config::last_good_snapshot_age().is_none(),
            "no snapshot means no age, not zero"
        );

        save_last_good_config();
        assert!(crate::config::last_good_config_path().exists());
        let age = crate::config::last_good_snapshot_age().expect("just written");
        assert!(
            age.as_secs() < 60,
            "a snapshot written a moment ago is not a minute old: {age:?}"
        );
    });
}
