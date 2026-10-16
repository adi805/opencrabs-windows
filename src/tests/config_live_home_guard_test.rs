//! Test builds must never write the live default home (#1399).

use std::path::Path;

use crate::config::live_home_guard::{refusal_for, refuse_live_home_write};

#[test]
fn a_file_directly_in_the_live_home_is_refused() {
    let home = Path::new("/Users/someone/.opencrabs");
    let reason = refusal_for(&home.join("config.toml"), home).expect("refused");
    assert!(reason.contains("config.toml"), "{reason}");
    assert!(reason.contains("with_home_override"), "{reason}");
    assert!(refusal_for(&home.join("keys.toml"), home).is_some());
}

#[test]
fn a_profile_scoped_or_temp_home_passes() {
    let home = Path::new("/Users/someone/.opencrabs");
    assert!(refusal_for(&home.join("profiles/test-x/config.toml"), home).is_none());
    assert!(refusal_for(Path::new("/tmp/abc/.opencrabs/config.toml"), home).is_none());
}

/// F-CFG-03: the guard used to test `target.parent() != Some(home)`, which is
/// true one directory down, so `~/.opencrabs/memory/…`, `~/.opencrabs/agents/…`
/// and `~/.opencrabs/rsi/…` were all writable from a test build. #1399 happened
/// to be a `config.toml` write, which is why the gap went unnoticed; the state
/// those deeper paths hold is just as live.
#[test]
fn a_file_under_the_live_home_is_refused_at_any_depth() {
    let home = Path::new("/Users/someone/.opencrabs");
    for nested in [
        "memory/2026-10-16.md",
        "agents/session/9f1c.json",
        "rsi/improvements.md",
        "rsi/history/2026-10-16.md",
        "logs/opencrabs.2026-10-16",
    ] {
        assert!(
            refusal_for(&home.join(nested), home).is_some(),
            "{nested} sits under the live home and must be refused"
        );
    }
    assert!(
        refusal_for(home, home).is_some(),
        "the home itself is not a write target"
    );
}

/// The exemption is the override mechanism, so it has to keep working at every
/// depth inside a profile: a test under `with_home_override` writes to
/// `profiles/<name>/…`, including nested paths like its own memory log.
#[test]
fn a_profile_home_is_exempt_at_any_depth() {
    let home = Path::new("/Users/someone/.opencrabs");
    for nested in [
        "profiles/test-x/config.toml",
        "profiles/test-x/memory/2026-10-16.md",
        "profiles/test-x/agents/session/9f1c.json",
    ] {
        assert!(
            refusal_for(&home.join(nested), home).is_none(),
            "{nested} is a profile home, which is the override, not the live one"
        );
    }
    // One level deep is the profiles DIRECTORY, not a profile home.
    assert!(
        refusal_for(&home.join("profiles"), home).is_some(),
        "profiles/ itself is not a home"
    );
}

/// The rule is component-wise, not a string prefix: a sibling directory whose
/// name merely starts with the home's must not be caught, or a test that runs in
/// `/tmp/x/.opencrabs-fixture` would be refused for no reason.
#[test]
fn a_sibling_of_the_live_home_is_not_caught_by_a_string_prefix() {
    let home = Path::new("/Users/someone/.opencrabs");
    assert!(refusal_for(Path::new("/Users/someone/.opencrabsX/config.toml"), home).is_none());
    assert!(
        refusal_for(
            Path::new("/Users/someone/.opencrabs-backup/config.toml"),
            home
        )
        .is_none()
    );
}

#[test]
fn the_guard_bites_in_a_test_build() {
    // This binary IS a test build, so the real base dir is off limits.
    let live = crate::config::profile::base_opencrabs_dir().join("config.toml");
    let err = refuse_live_home_write(&live).expect_err("must refuse");
    assert!(err.to_string().contains("test build"), "{err}");
}

/// The F-CFG-03 case, against the real base dir rather than a fixture path: the
/// memory log is the write that was left open, and it is the one the guard has
/// to stop now.
#[test]
fn the_guard_bites_on_a_nested_path_in_a_test_build() {
    let base = crate::config::profile::base_opencrabs_dir();
    for nested in ["memory/2026-10-16.md", "agents/session/9f1c.json"] {
        let target = base.join(nested);
        let err = refuse_live_home_write(&target)
            .expect_err(&format!("{nested} must be refused in a test build"));
        assert!(err.to_string().contains("refusing to write"), "{err}");
    }
    // And the exemption still holds where it must.
    let profile = base.join("profiles/test-x/memory/2026-10-16.md");
    refuse_live_home_write(&profile).expect("a profile home is the override");
}

#[test]
fn the_guard_lets_a_temp_home_through() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".opencrabs").join("config.toml");
    refuse_live_home_write(&target).expect("temp home is fine");
}

#[test]
fn atomic_write_into_the_live_home_is_refused_without_touching_it() {
    let live = crate::config::profile::base_opencrabs_dir().join("config.toml");
    let before = std::fs::metadata(&live).ok().map(|m| m.modified().ok());
    let err = crate::config::types::io::atomic_write(&live, "poison = true\n")
        .expect_err("the live config must not be rewritten by a test");
    assert!(err.to_string().contains("refusing to write"), "{err}");
    let after = std::fs::metadata(&live).ok().map(|m| m.modified().ok());
    assert_eq!(before, after, "the live file must be untouched");
}
