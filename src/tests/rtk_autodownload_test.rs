//! Tests for the RTK first-use auto-download wiring in `src/rtk/rewrite.rs`.
//!
//! These guard the two things that silently break the feature: the platform
//! to release-asset mapping, and the pinned RTK version drifting away from the
//! version the release pipeline bundles through `scripts/fetch-rtk.sh`.

use crate::rtk::rewrite::{RTK_VERSION, rtk_asset_name, rtk_bin_filename};

#[test]
fn current_platform_has_a_release_asset() {
    // Whatever platform the test runs on must map to a real RTK asset,
    // otherwise auto-download can never succeed there.
    let asset = rtk_asset_name();
    assert!(
        asset.is_some(),
        "no RTK release asset mapped for {}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
}

#[test]
fn asset_extension_matches_platform() {
    if let Some(asset) = rtk_asset_name() {
        if cfg!(windows) {
            assert!(
                asset.ends_with(".zip"),
                "windows asset must be a .zip: {asset}"
            );
        } else {
            assert!(
                asset.ends_with(".tar.gz"),
                "unix asset must be a .tar.gz: {asset}"
            );
        }
    }
}

#[test]
fn bin_filename_matches_platform() {
    if cfg!(windows) {
        assert_eq!(rtk_bin_filename(), "rtk.exe");
    } else {
        assert_eq!(rtk_bin_filename(), "rtk");
    }
}

#[test]
fn pinned_version_matches_the_fetch_script() {
    // The auto-download version must equal the version the release pipeline
    // bundles, so source builds install the same RTK prebuilt releases ship.
    // `scripts/fetch-rtk.sh` is the single trust anchor now, so that script is
    // what this compares against; reading a workflow here would only pin the
    // location the old copy happened to live in.
    let script =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/fetch-rtk.sh"))
            .expect("scripts/fetch-rtk.sh should exist at the repo root");

    let needle = "RTK_VERSION=\"";
    let start = script
        .find(needle)
        .map(|i| i + needle.len())
        .expect("scripts/fetch-rtk.sh should pin RTK_VERSION");
    let end = script[start..]
        .find('"')
        .map(|i| start + i)
        .expect("RTK_VERSION should be a quoted string");
    let script_version = &script[start..end];

    assert_eq!(
        script_version, RTK_VERSION,
        "rtk::RTK_VERSION ({RTK_VERSION}) drifted from scripts/fetch-rtk.sh ({script_version})"
    );
}

#[test]
fn workflows_do_not_reinline_the_rtk_version() {
    // One trust anchor: the version and the digests live in
    // `scripts/fetch-rtk.sh`, and every workflow calls it. Inline copies are
    // how the five old copies drifted apart, so a reappearing one is a
    // regression in the supply-chain property this file guards.
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/.github/workflows");
    let entries = std::fs::read_dir(dir).expect(".github/workflows should exist");
    let mut offenders = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_yaml = matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("yml" | "yaml")
        );
        if !is_yaml {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        if body.contains("RTK_VERSION=\"") {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            offenders.push(name);
        }
    }

    assert!(
        offenders.is_empty(),
        "workflows re-inline RTK_VERSION instead of calling scripts/fetch-rtk.sh: {offenders:?}"
    );
}
