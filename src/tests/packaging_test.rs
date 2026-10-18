//! Packaging assertions.
//!
//! The release tarballs are the only thing most users ever touch, and the
//! pipeline that builds them is not compiled, so a dropped line there is
//! invisible to `cargo test` unless something reads the workflow. These tests
//! pin the three packaging decisions that give the binary an icon: the macOS
//! app bundle, the Linux desktop entry, and the Homebrew cask scaffold.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let path = repo().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn the_release_workflow_bundles_the_macos_app() {
    let workflow = read(".github/workflows/release.yml");

    assert!(
        workflow.contains("scripts/package-macos-app.sh"),
        "the release workflow no longer invokes the macOS bundle script, so the tarball ships a bare Mach-O and macOS falls back to the generic exec glyph"
    );
    assert!(
        workflow.contains("--out dist"),
        "the bundle step does not write to dist/, so the archive step has nothing to pack"
    );
    assert!(
        workflow.contains("OpenCrabs.app"),
        "the archive step does not name OpenCrabs.app, so the bundle never reaches the tarball"
    );
    assert!(
        workflow.contains("contains(matrix.asset_suffix, 'macos')"),
        "the bundle step is not gated to macOS, so it would run on targets that have no bundle. The macOS rows in this matrix are `os: self-hosted`, so the gate must key off asset_suffix, never matrix.os"
    );
}

#[test]
fn the_bundle_script_is_executable() {
    let path = repo().join("scripts/package-macos-app.sh");
    let meta = std::fs::metadata(&path).expect("scripts/package-macos-app.sh is missing");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "scripts/package-macos-app.sh is not executable, so the workflow step fails with permission denied"
        );
    }

    let _ = Path::new(&path);
}
