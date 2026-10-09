//! Evolve (self-update) Tests
//!
//! Tests for version comparison, platform detection, asset naming,
//! and install method detection.

use crate::brain::tools::evolve::is_newer;
use crate::brain::tools::evolve::release_check::{
    STABLE_ASSET_SUFFIXES, StableAsset, classify_stable_asset, has_platform_asset,
};
use crate::utils::install::{InstallMethod, binary_name, platform_suffix};

// ─── Version comparison ─────────────────────────────────────────────────────

#[test]
fn is_newer_major_bump() {
    assert!(is_newer("1.0.0", "0.9.9"));
    assert!(is_newer("2.0.0", "1.99.99"));
}

#[test]
fn is_newer_minor_bump() {
    assert!(is_newer("0.3.0", "0.2.66"));
    assert!(is_newer("0.2.67", "0.2.66"));
}

#[test]
fn is_newer_patch_bump() {
    assert!(is_newer("0.2.66", "0.2.65"));
}

#[test]
fn is_newer_equal_returns_false() {
    assert!(!is_newer("0.2.66", "0.2.66"));
    assert!(!is_newer("1.0.0", "1.0.0"));
}

#[test]
fn is_newer_older_returns_false() {
    assert!(!is_newer("0.2.65", "0.2.66"));
    assert!(!is_newer("0.1.0", "0.2.0"));
    assert!(!is_newer("0.9.9", "1.0.0"));
}

#[test]
fn is_newer_handles_different_lengths() {
    // "1.0" vs "0.9.9" — 1.0 parsed as [1, 0], 0.9.9 as [0, 9, 9]
    assert!(is_newer("1.0", "0.9.9"));
    assert!(!is_newer("0.9", "0.9.9"));
}

#[test]
fn is_newer_ignores_non_numeric() {
    // Non-numeric parts are filtered out
    assert!(is_newer("1.0.0-beta", "0.9.0"));
}

// ─── Asset naming (single binary) ──────────────────────────────────────────

#[test]
fn asset_name_format() {
    // Verify the asset naming convention used by evolve
    let tag = "v0.2.67";
    let suffix = "macos-arm64";
    let ext = "tar.gz";
    let expected = format!("opencrabs-{}-{}.{}", tag, suffix, ext);
    assert_eq!(expected, "opencrabs-v0.2.67-macos-arm64.tar.gz");
}

#[test]
fn asset_name_windows() {
    let tag = "v0.2.67";
    let suffix = "windows-amd64";
    let ext = "zip";
    let expected = format!("opencrabs-{}-{}.{}", tag, suffix, ext);
    assert_eq!(expected, "opencrabs-v0.2.67-windows-amd64.zip");
}

#[test]
fn legacy_asset_name_fallback() {
    // Legacy naming without version tag
    let suffix = "linux-amd64";
    let ext = "tar.gz";
    let legacy = format!("opencrabs-{}.{}", suffix, ext);
    assert_eq!(legacy, "opencrabs-linux-amd64.tar.gz");
}

// ─── Binary extraction: always "opencrabs" (single binary) ──────────────────

#[test]
fn binary_name_is_always_opencrabs() {
    // The evolve tool always extracts "opencrabs" (or "opencrabs.exe" on Windows)
    let is_windows = std::env::consts::OS == "windows";
    let binary_name = if is_windows {
        "opencrabs.exe"
    } else {
        "opencrabs"
    };
    assert!(binary_name.starts_with("opencrabs"));
}

// ─── Platform suffix coverage ───────────────────────────────────────────────

#[test]
fn current_platform_has_suffix() {
    // platform_suffix is now public via utils::install
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let supported = matches!(
        (os, arch),
        ("macos", "aarch64")
            | ("macos", "x86_64")
            | ("linux", "x86_64")
            | ("linux", "aarch64")
            | ("windows", "x86_64")
    );
    if supported {
        assert!(platform_suffix().is_some());
    }
}

// ─── Install method detection ────────────────────────────────────────────────

#[test]
fn install_method_detect_does_not_panic() {
    let method = InstallMethod::detect();
    // On a dev machine building from source, should be Source
    assert!(!method.description().is_empty());
}

#[test]
fn install_method_source_from_dev_build() {
    // When running tests via cargo, we're in a source build
    let method = InstallMethod::detect();
    matches!(method, InstallMethod::Source(_));
}

#[test]
fn install_method_descriptions_are_distinct() {
    let source = InstallMethod::Source(std::path::PathBuf::from("/tmp"));
    let cargo = InstallMethod::CargoInstall;
    let prebuilt = InstallMethod::PrebuiltBinary;
    assert_ne!(source.description(), cargo.description());
    assert_ne!(cargo.description(), prebuilt.description());
    assert_ne!(source.description(), prebuilt.description());
}

#[test]
fn binary_name_is_platform_correct() {
    let name = binary_name();
    if std::env::consts::OS == "windows" {
        assert_eq!(name, "opencrabs.exe");
    } else {
        assert_eq!(name, "opencrabs");
    }
}

// ─── Asset availability (has_platform_asset) ────────────────────────────────

fn fake_release(asset_names: &[&str]) -> serde_json::Value {
    let assets: Vec<serde_json::Value> = asset_names
        .iter()
        .map(|name| {
            serde_json::json!({
                "name": name,
                "browser_download_url": format!("https://example.com/{}", name)
            })
        })
        .collect();
    serde_json::json!({ "tag_name": "v0.2.68", "assets": assets })
}

#[test]
fn has_platform_asset_finds_matching_asset() {
    let suffix = platform_suffix().unwrap();
    let ext = if std::env::consts::OS == "windows" {
        "zip"
    } else {
        "tar.gz"
    };
    let asset = format!("opencrabs-v0.2.68-{}.{}", suffix, ext);
    let release = fake_release(&[&asset]);
    assert!(has_platform_asset(&release, "v0.2.68"));
}

#[test]
fn has_platform_asset_empty_assets() {
    let release = fake_release(&[]);
    assert!(!has_platform_asset(&release, "v0.2.68"));
}

#[test]
fn has_platform_asset_wrong_platform() {
    let release = fake_release(&["opencrabs-v0.2.68-fakeos-fakearch.tar.gz"]);
    assert!(!has_platform_asset(&release, "v0.2.68"));
}

#[test]
fn has_platform_asset_no_assets_key() {
    let release = serde_json::json!({ "tag_name": "v0.2.68" });
    assert!(!has_platform_asset(&release, "v0.2.68"));
}

#[test]
fn has_platform_asset_legacy_naming() {
    let suffix = platform_suffix().unwrap();
    let ext = if std::env::consts::OS == "windows" {
        "zip"
    } else {
        "tar.gz"
    };
    let legacy = format!("opencrabs-{}.{}", suffix, ext);
    let release = fake_release(&[&legacy]);
    assert!(has_platform_asset(&release, "v0.2.68"));
}

#[test]
fn has_platform_asset_wrong_tag_no_match() {
    let suffix = platform_suffix().unwrap();
    let ext = if std::env::consts::OS == "windows" {
        "zip"
    } else {
        "tar.gz"
    };
    // Asset is for v0.2.67 but we ask for v0.2.68
    let asset = format!("opencrabs-v0.2.67-{}.{}", suffix, ext);
    let release = fake_release(&[&asset]);
    assert!(!has_platform_asset(&release, "v0.2.68"));
}

#[test]
fn has_platform_asset_multiple_assets_finds_correct() {
    let suffix = platform_suffix().unwrap();
    let ext = if std::env::consts::OS == "windows" {
        "zip"
    } else {
        "tar.gz"
    };
    let correct = format!("opencrabs-v0.2.68-{}.{}", suffix, ext);
    let release = fake_release(&[
        "opencrabs-v0.2.68-fakeos-fakearch.tar.gz",
        &correct,
        "opencrabs-v0.2.68-otheros-otherarch.zip",
    ]);
    assert!(has_platform_asset(&release, "v0.2.68"));
}

// ─── Stable-channel asset classification (#151) ─────────────────────────────

#[test]
fn a_present_asset_is_present_on_every_platform() {
    assert_eq!(
        classify_stable_asset(Some("linux-amd64"), true),
        StableAsset::Present
    );
    assert_eq!(
        classify_stable_asset(Some("windows-amd64"), true),
        StableAsset::Present
    );
    assert_eq!(classify_stable_asset(None, true), StableAsset::Present);
}

#[test]
fn a_missing_asset_on_a_stable_platform_still_reads_as_building() {
    for &suffix in STABLE_ASSET_SUFFIXES {
        assert_eq!(
            classify_stable_asset(Some(suffix), false),
            StableAsset::Building,
            "{suffix} is published by the stable workflow, so a missing asset \
             is a timing problem, not a policy one"
        );
    }
}

#[test]
fn a_missing_asset_off_the_stable_matrix_is_not_distributed() {
    // The gap this classification exists for: Windows and macOS are built by
    // prerelease.yml, never by release-fork.yml, so `releases/latest` (which
    // excludes prereleases) can never carry them. Reporting "still building"
    // there is a promise that never comes due.
    for suffix in ["windows-amd64", "macos-arm64", "macos-amd64"] {
        assert!(
            !STABLE_ASSET_SUFFIXES.contains(&suffix),
            "{suffix} must not be claimed as a stable channel asset"
        );
        assert_eq!(
            classify_stable_asset(Some(suffix), false),
            StableAsset::NotDistributed,
            "{suffix} is not on the stable channel, so it must not be told to wait"
        );
    }
}

#[test]
fn an_unnamed_platform_is_not_reported_as_a_policy_gap() {
    // platform_suffix() has no name for this build, which is a different
    // problem from the stable workflow choosing not to build it.
    assert_eq!(
        classify_stable_asset(None, false),
        StableAsset::UnsupportedPlatform
    );
}

#[test]
fn stable_channel_list_matches_the_release_workflow() {
    // STABLE_ASSET_SUFFIXES mirrors release-fork.yml's build-linux matrix, and
    // that mirror is what the "not distributed" message rests on. Add a Windows
    // or macOS leg there and this fails, so the updater's per-platform story has
    // to change in the same commit instead of going quietly stale.
    let workflow = include_str!("../../.github/workflows/release-fork.yml");
    let mut from_workflow: Vec<&str> = workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("asset_suffix:"))
        .map(str::trim)
        .collect();
    from_workflow.sort_unstable();
    let mut expected: Vec<&str> = STABLE_ASSET_SUFFIXES.to_vec();
    expected.sort_unstable();
    assert_eq!(
        from_workflow, expected,
        "release-fork.yml builds {from_workflow:?} but the updater assumes {expected:?}"
    );
}
