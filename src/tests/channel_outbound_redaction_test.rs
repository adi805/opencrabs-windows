//! #2102: the LLM-artifact strip and the secret redactor were paired by
//! convention only. Twenty-five outbound sites under `src/channels/` called
//! `strip_llm_artifacts`, twenty-one called `redact_secrets`, and the eight
//! that did not delivered text that was artifact-clean but still carried any
//! credential-shaped string the model had echoed.
//!
//! Two invariants are pinned here:
//!
//! 1. `sanitize_outbound` masks a credential-shaped string and leaves
//!    ordinary prose untouched.
//! 2. Every `strip_llm_artifacts` call site under `src/channels/` either goes
//!    through `sanitize_outbound` or has a `redact_secrets` call within
//!    `PAIRING_WINDOW` lines. A path written as a bare strip fails here
//!    instead of shipping unredacted, which is the guard the issue asked for.

use crate::utils::sanitize::sanitize_outbound;
use std::fs;
use std::path::{Path, PathBuf};

/// How close a `redact_secrets` call has to sit to count as paired. The
/// paired sites all call it on the next line; the window only tolerates a
/// few lines of intervening work.
const PAIRING_WINDOW: usize = 25;

#[test]
fn sanitize_outbound_masks_a_credential() {
    let text = "deploy finished with sk-proj-AbCdEf0123456789AbCdEf0123456789 attached";
    let out = sanitize_outbound(text);
    assert!(
        !out.contains("sk-proj-AbCdEf0123456789AbCdEf0123456789"),
        "the credential survived the outbound boundary: {out}"
    );
}

#[test]
fn sanitize_outbound_leaves_ordinary_prose_alone() {
    let text = "The build finished in 42 seconds and the tests passed.";
    assert_eq!(
        sanitize_outbound(text),
        text,
        "the outbound boundary must be a no-op on prose with no secret and no artifact"
    );
}

#[test]
fn every_strip_site_under_channels_also_redacts() {
    let channels = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("channels");
    let mut files = Vec::new();
    collect_rs_files(&channels, &mut files);
    assert!(
        files.len() > 20,
        "source walk found nothing under {:?}; cwd={:?}",
        channels,
        std::env::current_dir()
    );

    let mut violations = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(channels.parent().expect("channels has a parent"))
            .expect("walk root is under src")
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        let lines: Vec<&str> = content.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("strip_llm_artifacts") || line.trim_start().starts_with("use ") {
                continue;
            }
            let lo = i.saturating_sub(PAIRING_WINDOW);
            let hi = (i + PAIRING_WINDOW + 1).min(lines.len());
            let window = lines[lo..hi].join("\n");
            if !window.contains("redact_secrets") && !window.contains("sanitize_outbound") {
                violations.push(format!("{rel}:{}", i + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "outbound strip sites with no redaction within {PAIRING_WINDOW} lines: {violations:?}"
    );
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
