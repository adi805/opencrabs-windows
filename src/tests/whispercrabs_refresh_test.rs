//! #2114: `/whisper` provisioned WhisperCrabs with an `exists()` check and
//! nothing else, so a cached binary was never refreshed: the first release
//! after an install stranded every user who had ever run the command, with no
//! API call and no message.
//!
//! The decision is pinned here as a pure function, so no network and no home
//! directory are involved. The sidecar helpers are exercised against a
//! `tempfile` dir, never the live `~/.opencrabs` (a test binary that writes the
//! real home is how #1399 disabled voice on every `cargo test`).

use crate::tui::app::dialogs::{
    cached_binary_ok, needs_refresh, read_cached_tag, tag_path, write_cached_tag,
};

#[test]
fn no_cache_needs_a_download() {
    assert!(needs_refresh(None, "v0.1.24", false));
}

#[test]
fn a_matching_tag_and_a_healthy_file_is_reused() {
    assert!(!needs_refresh(Some("v0.1.23"), "v0.1.23", true));
}

#[test]
fn a_newer_release_refreshes_the_cache() {
    assert!(needs_refresh(Some("v0.1.23"), "v0.1.24", true));
}

#[test]
fn an_unrecorded_tag_is_treated_as_stale() {
    // An install from before the sidecar existed: there is no way to know
    // which release the file is, so it is replaced once and recorded.
    assert!(needs_refresh(None, "v0.1.23", true));
}

#[test]
fn a_broken_file_is_refreshed_even_when_the_tag_matches() {
    for ok in [false] {
        assert!(
            needs_refresh(Some("v0.1.23"), "v0.1.23", ok),
            "a zero-byte or non-executable cache must not be handed to Command::new"
        );
    }
}

#[test]
fn a_zero_byte_or_missing_file_is_not_usable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("whispercrabs");
    assert!(!cached_binary_ok(&missing), "a missing file is not usable");

    std::fs::write(&missing, b"").expect("write empty");
    assert!(
        !cached_binary_ok(&missing),
        "a zero-byte file is not usable"
    );
}

#[cfg(unix)]
#[test]
fn a_non_executable_file_is_not_usable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = dir.path().join("whispercrabs");
    std::fs::write(&binary, b"#!/bin/sh\n").expect("write");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(!cached_binary_ok(&binary), "mode 644 cannot be executed");

    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(cached_binary_ok(&binary), "mode 755 is usable");
}

#[test]
fn the_tag_sidecar_round_trips_next_to_the_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = dir.path().join("whispercrabs");
    assert_eq!(
        tag_path(&binary).file_name().and_then(|n| n.to_str()),
        Some("whispercrabs.version")
    );
    assert_eq!(read_cached_tag(&binary), None, "no sidecar means no tag");

    write_cached_tag(&binary, "v0.1.23");
    assert_eq!(read_cached_tag(&binary).as_deref(), Some("v0.1.23"));

    write_cached_tag(&binary, "v0.1.24");
    assert_eq!(read_cached_tag(&binary).as_deref(), Some("v0.1.24"));
}
