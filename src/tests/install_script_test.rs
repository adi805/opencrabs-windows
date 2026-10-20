// Witness for #2142: the one-line installer is what an agent pipes into bash,
// so it must be safe to run with nobody watching. Two defects lived here.
//
// 1. It could block forever on a sudo password prompt. An agent has no
//    terminal, so the prompt never gets answered and the run hangs with no
//    timeout. It now fails fast and names the writable-dir fix instead.
// 2. On macOS it built the asset name from `uname -s`, which is `Darwin`, so
//    it asked for `opencrabs-v0.5.11-darwin-arm64.tar.gz`. Every release
//    publishes `macos-arm64`, so the download 404'd and the installer had
//    never worked on a Mac. Confirmed against the real v0.5.11 assets.
//
// The checks are structural because the script talks to GitHub, which no test
// should depend on. Anything provable without a network belongs here.

use std::fs;

const SCRIPT: &str = "src/scripts/install.sh";

fn script() -> String {
    fs::read_to_string(SCRIPT).expect("install.sh readable")
}

/// Code lines only, so the doc comment explaining the rule cannot trip it.
fn code_lines() -> Vec<String> {
    script()
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with('#') && !t.is_empty()
        })
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn the_installer_parses_as_bash() {
    let out = std::process::Command::new("bash")
        .arg("-n")
        .arg(SCRIPT)
        .output()
        .expect("bash available");
    assert!(
        out.status.success(),
        "install.sh does not parse: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_installer_never_waits_for_input() {
    // `read` blocks on a pipe that never sends anything. A prompt is the same
    // hang wearing a friendlier face.
    for line in code_lines() {
        let t = line.trim();
        assert!(
            !t.starts_with("read "),
            "install.sh calls `read`, which blocks forever with no stdin: {line}"
        );
        assert!(
            !t.contains("read -p") && !t.contains("read -r"),
            "install.sh prompts for input, which hangs an unattended run: {line}"
        );
    }
}

#[test]
fn sudo_never_prompts_unless_a_terminal_is_present() {
    // A bare `sudo` reads /dev/tty. With no terminal that is a wait that never
    // ends. It is allowed only on the branch guarded by the INTERACTIVE check.
    let src = script();
    assert!(
        src.contains("INTERACTIVE=1"),
        "install.sh must decide whether a terminal exists before using sudo"
    );
    assert!(
        src.contains("sudo -n"),
        "the unattended path must use `sudo -n`, which fails instead of prompting"
    );

    for line in code_lines() {
        let t = line.trim();
        // Only a line that STARTS with sudo is a command. "passwordless sudo
        // is not available" is prose inside an error message.
        if !t.starts_with("sudo ") || t.starts_with("sudo -n") {
            continue;
        }
        // The only permitted bare sudo is inside the as_root() interactive arm,
        // which is unreachable when INTERACTIVE=0.
        assert!(
            t == "sudo \"$@\"",
            "install.sh runs a bare `sudo` outside the guarded interactive arm, \
             which hangs an unattended run: {line}"
        );
    }
    assert!(
        src.contains("if [ -t 2 ] && [ -r /dev/tty ]"),
        "the interactive decision must check for a real terminal, not just stdin \
         (stdin is the curl pipe in `curl | bash`)"
    );
}

#[test]
fn the_macos_asset_name_matches_what_the_releases_publish() {
    // The regression that made macOS installs impossible: `uname -s` says
    // Darwin, the release assets say macos.
    let src = script();
    assert!(
        src.contains(r#"darwin) OS="macos""#),
        "install.sh must map darwin to macos before building the asset name, or \
         the download 404s on every Mac"
    );
    // The asset name must be built from the mapped value. The releases publish
    // macos-*, so a literal darwin anywhere in a filename is the old bug.
    assert!(
        src.contains(r#"FILENAME="opencrabs-${TAG}-${OS}-${ARCH}.tar.gz""#),
        "the asset name must come from the mapped OS, not from uname"
    );
    assert!(
        !src.contains("darwin-${ARCH}") && !src.contains("darwin-amd64"),
        "install.sh builds a darwin asset name, which no release publishes"
    );
}

#[test]
fn the_installer_verifies_the_download_before_unpacking() {
    let src = script();
    let verify = src
        .find("SHA256SUMS")
        .expect("install.sh must fetch SHA256SUMS");
    let extract = src
        .find("tar xzf")
        .expect("install.sh must extract the archive");
    assert!(
        verify < extract,
        "the checksum must be checked before the archive is unpacked, otherwise \
         a corrupt download is already on disk when it fails"
    );
    assert!(
        src.contains("Checksum mismatch"),
        "a mismatch must be a hard failure with a named reason"
    );
}

#[test]
fn the_installer_installs_the_helper_binary_beside_the_agent() {
    // Every archive ships rtk next to opencrabs. Installing only the agent
    // leaves a feature flag promising a helper that is not on disk.
    let src = script();
    assert!(
        src.contains("install -m 755 \"${TMPDIR}/rtk\""),
        "install.sh must install rtk, which every release archive ships"
    );
}

#[test]
fn the_installer_handles_both_archive_layouts() {
    // Up to v0.5.11 the macOS archive carries a loose binary. #2115 changed it
    // to OpenCrabs.app, so the installer has to accept either rather than
    // assume the one that happens to be current.
    let src = script();
    assert!(
        src.contains("OpenCrabs.app"),
        "install.sh must handle the bundled macOS layout from #2115"
    );
    assert!(
        src.contains("\"${TMPDIR}/opencrabs\""),
        "install.sh must still handle the loose-binary layout of older releases"
    );
}

#[test]
fn the_installer_offers_a_dry_run_and_a_dependency_opt_out() {
    let src = script();
    for flag in ["--dry-run", "--no-deps", "--dir", "--version"] {
        assert!(
            src.contains(flag),
            "install.sh must accept {flag}: a caller that cannot inspect what \
             will happen before it happens is a caller that cannot verify"
        );
    }
    // --dry-run must exit before anything is written.
    let dry = src.find("Dry run: would install").expect("dry run branch");
    let download = src.find("Downloading ${TAG}").expect("download step");
    assert!(
        dry < download,
        "the dry run must exit before the download, not after it"
    );
}
