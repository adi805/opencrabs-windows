//! The Android workflows must rebuild when the core changes.
//!
//! `android-artifact.yml` and `android-emulator-smoke.yml` are path-filtered so
//! they do not fire on changes that cannot affect the Android client. But the
//! APK is not only the Android shell: it bundles a core built from this repo
//! (`cargo ndk`, then renamed to `libopencrabs.so` and staged into the APK), so
//! a change under `src/**` alters the shipped artifact while touching nothing
//! under `android/`.
//!
//! That gap is not hypothetical. `android-artifact.yml` records the 2026-10-06
//! incident where the on-disk APK predated a merged core fix, and adding `main`
//! to `branches` did not close it: a core-only fix landing on `main` still
//! matched none of the `paths`.
//!
//! These tests read the YAML rather than trusting it, so the filter is asserted
//! by something that runs in CI instead of by a comment.

const WORKFLOWS: [&str; 2] = ["android-artifact.yml", "android-emulator-smoke.yml"];

/// Paths that must be in the filter because the artifact contains the core.
const REQUIRED_CORE_PATHS: [&str; 3] = ["src/**", "Cargo.toml", "Cargo.lock"];

fn workflow(name: &str) -> String {
    let path = format!("{}/.github/workflows/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// Every entry of the `paths:` list under `on.push`, in order.
///
/// Deliberately a small hand-rolled reader rather than a YAML dependency: it
/// only has to survive the shape these two files use. A parser that quietly
/// returns nothing is the failure mode worth guarding against, so the callers
/// assert the list is non-empty and the reader itself is pinned by fixtures.
fn push_paths(yaml: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut in_paths = false;
    // Only the FIRST `paths:` block counts: it is the one under `on.push`,
    // and a later one (a job's own `paths:`, or a second trigger) must not
    // leak into the answer. Pinned by
    // `the_filter_reader_finds_the_push_paths_it_is_given`.
    let mut found = false;
    let mut paths_indent = 0usize;
    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if in_paths {
            if indent > paths_indent {
                if let Some(rest) = trimmed.strip_prefix("- ") {
                    paths.push(rest.trim().trim_matches(['\'', '"']).to_string());
                }
                continue;
            }
            in_paths = false;
        }
        if !found && trimmed == "paths:" {
            in_paths = true;
            found = true;
            paths_indent = indent;
        }
    }
    paths
}

#[test]
fn the_filter_reader_finds_the_push_paths_it_is_given() {
    // A parser that silently matches nothing would make every assertion below
    // vacuous, so pin the reader against a fixture with a known answer. The
    // second `paths:` under `jobs:` must NOT leak into the result.
    let fixture = "\
on:
  push:
    branches: [main]
    paths:
      - 'android/**'
      - 'src/**'
jobs:
  build:
    paths:
      - 'not-this-one/**'
";
    assert_eq!(push_paths(fixture), vec!["android/**", "src/**"]);
}

#[test]
fn the_filter_reader_finds_nothing_without_a_paths_key() {
    assert!(push_paths("on:\n  push:\n    branches: [main]\n").is_empty());
}

#[test]
fn android_workflows_rebuild_for_core_changes() {
    for name in WORKFLOWS {
        let paths = push_paths(&workflow(name));
        assert!(
            !paths.is_empty(),
            "{name}: no push paths filter found; if the YAML shape changed, \
             update push_paths() rather than dropping this check"
        );
        for required in REQUIRED_CORE_PATHS {
            assert!(
                paths.iter().any(|p| p == required),
                "{name}: the paths filter omits '{required}', so a change to it \
                 would not rebuild the Android artifact even though the APK \
                 bundles a core built from this repo. Filter: {paths:?}"
            );
        }
    }
}

#[test]
fn android_workflows_keep_their_android_paths() {
    // Widening the filter must not REPLACE what was already there: a change to
    // the Android shell still has to trigger a rebuild.
    for name in WORKFLOWS {
        let paths = push_paths(&workflow(name));
        assert!(
            paths.iter().any(|p| p == "android/**"),
            "{name}: 'android/**' was dropped from the filter while adding the \
             core paths. Filter: {paths:?}"
        );
    }
}
