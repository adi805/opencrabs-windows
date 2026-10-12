//! #214: the Cargo update and rollback paths must select the fork package, not
//! the upstream crate, and must not conflate package identity with the
//! executable name.
//!
//! Upstream publishes `opencrabs`; this fork publishes `opencrabs-fork` while
//! keeping the `opencrabs` binary name. A hardcoded `cargo install opencrabs`
//! therefore installs a *different distribution* and, because the command
//! still exits zero, reports it as a successful update.
//!
//! These tests pin the shared constant and the two call sites that must use
//! it. They read the source rather than the running process, so they hold
//! without a cargo toolchain on the test host.

use crate::utils::update_source::DEFAULT_UPDATE_PACKAGE;

const CARGO_TOML: &str = include_str!("../../Cargo.toml");
const VIA_CARGO_INSTALL: &str = include_str!("../brain/tools/evolve/via_cargo_install.rs");
const CRASH_RECOVERY: &str = include_str!("../cli/crash_recovery.rs");

/// The distribution package name comes from the manifest, so it cannot drift
/// from what this build actually is.
#[test]
fn update_package_is_the_manifest_package_name() {
    assert_eq!(
        DEFAULT_UPDATE_PACKAGE,
        env!("CARGO_PKG_NAME"),
        "the update package must be the manifest package name"
    );
}

/// The package is the fork. If this fails because the manifest changed, the
/// update paths now resolve a different distribution and the constant moved
/// with it, which is the intended coupling.
#[test]
fn update_package_is_the_fork_not_upstream() {
    assert_eq!(DEFAULT_UPDATE_PACKAGE, "opencrabs-fork");
    assert!(
        CARGO_TOML.contains("name = \"opencrabs-fork\""),
        "Cargo.toml no longer declares the fork package name"
    );
    assert_ne!(
        DEFAULT_UPDATE_PACKAGE, "opencrabs",
        "the update package must not be the upstream crate name"
    );
}

/// Both Cargo paths reference the shared constant and neither names the
/// upstream crate as a literal, so a future edit cannot silently re-select it.
#[test]
fn both_cargo_paths_use_the_shared_package_constant() {
    for (name, src) in [
        ("via_cargo_install.rs", VIA_CARGO_INSTALL),
        ("crash_recovery.rs", CRASH_RECOVERY),
    ] {
        assert!(
            src.contains("DEFAULT_UPDATE_PACKAGE"),
            "{name} does not reference DEFAULT_UPDATE_PACKAGE"
        );
        assert!(
            !src.contains("\"install\", \"opencrabs\""),
            "{name} still passes the upstream crate name to `cargo install`"
        );
    }
}

/// Binary identity stays `opencrabs` on every platform. Fixing the package
/// name must not rename the executable, which is what the issue calls out as
/// the thing not to conflate.
#[test]
fn binary_name_stays_opencrabs() {
    let name = crate::utils::install::binary_name();
    assert!(
        name.starts_with("opencrabs"),
        "binary name changed unexpectedly: {name}"
    );
    assert!(
        !name.contains("fork"),
        "binary name must not carry the package identity: {name}"
    );
}

/// The update path asks cargo for the version it intends to install, and does
/// not report success before reading back what was installed.
#[test]
fn cargo_update_passes_the_intended_version_and_verifies_it() {
    assert!(
        VIA_CARGO_INSTALL.contains("\"--version\""),
        "the update path does not pass the intended version to cargo"
    );
    assert!(
        VIA_CARGO_INSTALL.contains("installed_binary_version"),
        "the update path does not verify the installed version afterwards"
    );
    assert!(
        VIA_CARGO_INSTALL.contains("Refusing to signal a restart"),
        "a failed verification must not reach the restart signal"
    );
}
