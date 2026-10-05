//! Tests for the pruned-sections sidecar at
//! `~/.opencrabs/rsi/pruned.toml`.
//!
//! Originally lived as an inline `#[cfg(test)] mod tests` block in
//! `src/brain/rsi_pruned.rs`; moved here per project policy that every
//! test is a file under `src/tests/` registered in `mod.rs`.
//!
//! Coverage:
//! - Parse / save roundtrip
//! - Empty input, escaped quotes, basic two-file shape
//! - `record_pruned` deduping, `clear` per-file and global
//! - `detect_removed_sections` diff
//! - Issue #164 follow-up: `schema_version` field on v2 sidecars
//! - Issue #164 follow-up: `moved_headers` support via `MovedEntry`,
//!   `record_moved`, `moved_destination`, and the filter integration
//!   (moved-source headers don't get re-added on sync)

use crate::brain::rsi_pruned::{
    MovedEntry, PrunedState, SCHEMA_VERSION, detect_removed_sections, filter_pruned_sections,
};

#[test]
fn parse_empty_returns_default_with_schema_v1() {
    let state = PrunedState::parse("");
    assert!(state.pruned.is_empty());
    assert!(state.pruned_at.is_empty());
    assert!(state.moved.is_empty());
    assert_eq!(
        state.schema_version, 1,
        "an unversioned (empty) sidecar must parse as v1 so the next save() upgrades it"
    );
}

#[test]
fn parse_basic_two_file_shape() {
    let toml = "[SOUL.md]\npruned = [\"## Old Section\", \"### Old Subsection\"]\n\
                pruned_at = \"2026-06-04T10:00:00Z\"\n\n\
                [TOOLS.md]\npruned = [\"## Deprecated Docs\"]\n";
    let state = PrunedState::parse(toml);
    assert_eq!(state.pruned.len(), 2);
    assert_eq!(
        state.pruned.get("SOUL.md").unwrap(),
        &vec!["## Old Section", "### Old Subsection"]
    );
    assert_eq!(
        state.pruned_at.get("SOUL.md").unwrap(),
        "2026-06-04T10:00:00Z"
    );
    assert_eq!(
        state.pruned.get("TOOLS.md").unwrap(),
        &vec!["## Deprecated Docs"]
    );
}

#[test]
fn parse_escaped_quotes() {
    let toml = "[SOUL.md]\npruned = [\"## Section with \\\"quotes\\\"\"]\n";
    let state = PrunedState::parse(toml);
    let headers = state.pruned.get("SOUL.md").unwrap();
    assert_eq!(headers, &vec!["## Section with \"quotes\""]);
}

#[test]
fn is_pruned_lookup() {
    let mut state = PrunedState::default();
    state.record_pruned("SOUL.md", vec!["## Old".to_string()]);
    assert!(state.is_pruned("SOUL.md", "## Old"));
    assert!(!state.is_pruned("SOUL.md", "## New"));
    assert!(!state.is_pruned("TOOLS.md", "## Old"));
}

#[test]
fn record_pruned_dedupes_across_calls() {
    let mut state = PrunedState::default();
    state.record_pruned("SOUL.md", vec!["## A".to_string(), "## B".to_string()]);
    state.record_pruned("SOUL.md", vec!["## B".to_string(), "## C".to_string()]);
    let headers = state.pruned.get("SOUL.md").unwrap();
    assert_eq!(headers, &vec!["## A", "## B", "## C"]);
}

#[test]
fn clear_per_file() {
    let mut state = PrunedState::default();
    state.record_pruned("SOUL.md", vec!["## A".to_string()]);
    state.record_pruned("TOOLS.md", vec!["## B".to_string()]);
    state.clear(Some("SOUL.md"));
    assert!(!state.pruned.contains_key("SOUL.md"));
    assert!(state.pruned.contains_key("TOOLS.md"));
}

#[test]
fn clear_all() {
    let mut state = PrunedState::default();
    state.record_pruned("SOUL.md", vec!["## A".to_string()]);
    state.record_pruned("TOOLS.md", vec!["## B".to_string()]);
    state.clear(None);
    assert!(state.pruned.is_empty());
}

#[test]
fn clear_also_drops_moved_entries_for_same_scope() {
    let mut state = PrunedState::default();
    state.record_moved(
        "SOUL.md",
        vec![MovedEntry {
            header: "## A".to_string(),
            dest: "AGENTS.md".to_string(),
        }],
    );
    state.record_moved(
        "TOOLS.md",
        vec![MovedEntry {
            header: "## B".to_string(),
            dest: "MEMORY.md".to_string(),
        }],
    );
    state.clear(Some("SOUL.md"));
    assert!(
        !state.moved.contains_key("SOUL.md"),
        "per-file clear must drop moved entries too, not just pruned"
    );
    assert!(state.moved.contains_key("TOOLS.md"));
    state.clear(None);
    assert!(state.moved.is_empty(), "global clear must wipe moved too");
}

#[test]
fn detect_removed_sections_diff() {
    let old = "# Title\n\n## Keep\ncontent\n\n## Remove Me\nold stuff\n\n### Also Removed\n";
    let new = "# Title\n\n## Keep\ncontent\n\n## New Section\n";
    let removed = detect_removed_sections(old, new);
    assert_eq!(removed, vec!["## Remove Me", "### Also Removed"]);
}

#[test]
fn detect_removed_sections_no_changes() {
    let content = "# Title\n\n## Keep\ncontent\n";
    let removed = detect_removed_sections(content, content);
    assert!(removed.is_empty());
}

#[test]
fn save_round_trip_with_tempdir() {
    use std::fs;
    let tmp = tempfile::TempDir::new().expect("create tempdir for sidecar roundtrip test");
    let path = tmp.path().join("pruned.toml");

    let mut state = PrunedState::default();
    state.record_pruned("SOUL.md", vec!["## Old Section".to_string()]);

    // Emulate `save()` formatting just for this file (the real save()
    // writes to the user's actual home, which the test must not touch).
    let mut content = String::from("[SOUL.md]\npruned = [");
    let escaped: Vec<String> = state
        .pruned
        .get("SOUL.md")
        .unwrap()
        .iter()
        .map(|h| format!("\"{}\"", h.replace('"', "\\\"")))
        .collect();
    content.push_str(&escaped.join(", "));
    content.push_str("]\n");
    fs::write(&path, &content).expect("write sidecar fixture");

    let loaded = PrunedState::parse(&fs::read_to_string(&path).expect("read sidecar fixture"));
    assert_eq!(
        loaded.pruned.get("SOUL.md").unwrap(),
        &vec!["## Old Section"]
    );
    // tempfile::TempDir cleans up on drop with error handling baked in,
    // so no `let _ = std::fs::remove_dir_all(&tmp);` silent teardown.
}

// ── Issue #164 follow-up: schema_version ─────────────────────────────

#[test]
fn schema_version_constant_is_at_least_v2() {
    const { assert!(SCHEMA_VERSION >= 2) }
}

#[test]
fn parse_recognises_schema_version_line() {
    let toml = "schema_version = 2\n\n[SOUL.md]\npruned = [\"## A\"]\n";
    let state = PrunedState::parse(toml);
    assert_eq!(state.schema_version, 2);
    assert_eq!(state.pruned.get("SOUL.md").unwrap(), &vec!["## A"]);
}

#[test]
fn parse_falls_back_to_v1_when_no_schema_version_line() {
    let toml = "[SOUL.md]\npruned = [\"## A\"]\n";
    let state = PrunedState::parse(toml);
    assert_eq!(
        state.schema_version, 1,
        "no schema_version line means it's a v1 sidecar from before the field existed"
    );
}

#[test]
fn parse_ignores_malformed_schema_version_value() {
    let toml = "schema_version = not_a_number\n\n[SOUL.md]\npruned = [\"## A\"]\n";
    let state = PrunedState::parse(toml);
    // We treat malformed as v1 (fallback) rather than panicking, so a
    // hand-edited bad line doesn't crash the loader.
    assert_eq!(state.schema_version, 1);
    assert_eq!(state.pruned.get("SOUL.md").unwrap(), &vec!["## A"]);
}

// ── Issue #164 follow-up: moved_headers ──────────────────────────────

#[test]
fn record_moved_appends_and_dedupes() {
    let mut state = PrunedState::default();
    state.record_moved(
        "SOUL.md",
        vec![
            MovedEntry {
                header: "## A".to_string(),
                dest: "AGENTS.md".to_string(),
            },
            MovedEntry {
                header: "## B".to_string(),
                dest: "MEMORY.md".to_string(),
            },
        ],
    );
    state.record_moved(
        "SOUL.md",
        vec![
            // Duplicate header — must NOT add a second entry, even if dest differs.
            MovedEntry {
                header: "## A".to_string(),
                dest: "DIFFERENT.md".to_string(),
            },
            MovedEntry {
                header: "## C".to_string(),
                dest: "TOOLS.md".to_string(),
            },
        ],
    );
    let entries = state.moved.get("SOUL.md").unwrap();
    assert_eq!(entries.len(), 3, "dedup by header must keep A/B/C");
    assert_eq!(entries[0].header, "## A");
    assert_eq!(entries[0].dest, "AGENTS.md");
    assert_eq!(entries[1].header, "## B");
    assert_eq!(entries[2].header, "## C");
}

#[test]
fn record_moved_noop_on_empty_input() {
    let mut state = PrunedState::default();
    state.record_moved("SOUL.md", vec![]);
    assert!(state.moved.is_empty());
    assert!(state.pruned_at.is_empty());
}

#[test]
fn moved_destination_lookup() {
    let mut state = PrunedState::default();
    state.record_moved(
        "SOUL.md",
        vec![MovedEntry {
            header: "## Identity".to_string(),
            dest: "AGENTS.md".to_string(),
        }],
    );
    assert_eq!(
        state.moved_destination("SOUL.md", "## Identity"),
        Some("AGENTS.md")
    );
    assert_eq!(state.moved_destination("SOUL.md", "## Unknown"), None);
    assert_eq!(state.moved_destination("TOOLS.md", "## Identity"), None);
}

#[test]
fn filter_pruned_sections_skips_moved_source_headers() {
    // The pruned + moved lists both contribute to "do not re-add" rules
    // in filter_pruned_sections. A moved header on the SOURCE side must
    // be skipped just like a pruned one.
    let mut state = PrunedState::default();
    state.record_moved(
        "SOUL.md",
        vec![MovedEntry {
            header: "## Identity".to_string(),
            dest: "AGENTS.md".to_string(),
        }],
    );
    let new_sections = "\n## Identity\nidentity body that the user moved to AGENTS\n\n## Stays\nthis one is new and must come through\n";
    let filtered = filter_pruned_sections(new_sections, &state, "SOUL.md");
    assert!(
        !filtered.contains("## Identity"),
        "moved-source header must be filtered out on the SOURCE file's sync. \
         Got filtered output:\n{}",
        filtered
    );
    assert!(
        filtered.contains("## Stays"),
        "non-moved sections must come through normally; got:\n{}",
        filtered
    );
}

#[test]
fn filter_pruned_sections_passes_through_when_no_pruned_or_moved() {
    let state = PrunedState::default();
    let new_sections = "\n## A\nbody a\n\n## B\nbody b\n";
    let filtered = filter_pruned_sections(new_sections, &state, "SOUL.md");
    assert_eq!(
        filtered, new_sections,
        "no pruned and no moved entries means pass-through"
    );
}

#[test]
fn parse_recognises_moved_array_single_line() {
    let toml = "schema_version = 2\n\n[SOUL.md]\nmoved = [[\"## A\", \"AGENTS.md\"], [\"## B\", \"MEMORY.md\"]]\n";
    let state = PrunedState::parse(toml);
    let entries = state.moved.get("SOUL.md").expect("SOUL.md moved entries");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].header, "## A");
    assert_eq!(entries[0].dest, "AGENTS.md");
    assert_eq!(entries[1].header, "## B");
    assert_eq!(entries[1].dest, "MEMORY.md");
}

#[test]
fn parse_recognises_moved_array_multi_line() {
    let toml = "schema_version = 2\n\n\
                [SOUL.md]\n\
                moved = [\n\
                  [\"## A\", \"AGENTS.md\"],\n\
                  [\"## B\", \"MEMORY.md\"],\n\
                ]\n";
    let state = PrunedState::parse(toml);
    let entries = state.moved.get("SOUL.md").expect("SOUL.md moved entries");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].header, "## A");
    assert_eq!(entries[1].dest, "MEMORY.md");
}

#[test]
fn parse_skips_malformed_moved_pair() {
    // Three-element inner array is malformed for a pair. Parser must
    // skip it without crashing, and other good entries must survive.
    let toml = "schema_version = 2\n\n\
                [SOUL.md]\n\
                moved = [[\"## A\", \"AGENTS.md\", \"EXTRA\"], [\"## B\", \"MEMORY.md\"]]\n";
    let state = PrunedState::parse(toml);
    let entries = state.moved.get("SOUL.md").expect("SOUL.md moved entries");
    assert_eq!(entries.len(), 1, "malformed pair must be dropped");
    assert_eq!(entries[0].header, "## B");
}

// --- toml_keys: dotted TOML paths the user deleted -------------------------
//
// The prose `pruned` list cannot express this: TOML merges by key, so a
// deleted `[providers.zai]` table is indistinguishable from one the user
// never had, and the additive merge restores it on every sync.

#[test]
fn parse_reads_toml_keys() {
    let toml = "schema_version = 2\n\n\
                [config.toml]\n\
                toml_keys = [\"providers.zai\", \"providers.custom.lm_studio\"]\n";
    let state = PrunedState::parse(toml);
    assert_eq!(
        state.toml_keys.get("config.toml").unwrap(),
        &vec![
            "providers.zai".to_string(),
            "providers.custom.lm_studio".to_string()
        ]
    );
}

#[test]
fn parse_defaults_toml_keys_to_empty() {
    let state = PrunedState::parse("[SOUL.md]\npruned = [\"## A\"]\n");
    assert!(state.toml_keys.is_empty());
}

#[test]
fn record_toml_keys_dedupes_across_calls() {
    let mut state = PrunedState::default();
    state.record_toml_keys("config.toml", vec!["providers.zai".to_string()]);
    state.record_toml_keys(
        "config.toml",
        vec![
            "providers.zai".to_string(),
            "providers.custom.lm_studio".to_string(),
        ],
    );
    let keys = state.toml_keys_for("config.toml");
    assert_eq!(
        keys,
        vec![
            "providers.zai".to_string(),
            "providers.custom.lm_studio".to_string()
        ],
        "duplicate must not be stored twice"
    );
}

#[test]
fn record_toml_keys_noop_on_empty_input() {
    let mut state = PrunedState::default();
    state.record_toml_keys("config.toml", Vec::new());
    assert!(state.toml_keys.is_empty());
    assert!(
        state.pruned_at.is_empty(),
        "an empty record must not stamp pruned_at"
    );
}

#[test]
fn toml_keys_for_unknown_file_is_empty() {
    let state = PrunedState::default();
    assert!(state.toml_keys_for("config.toml").is_empty());
}

#[test]
fn clear_per_file_drops_toml_keys() {
    let mut state = PrunedState::default();
    state.record_toml_keys("config.toml", vec!["providers.zai".to_string()]);
    state.record_toml_keys("tools.toml", vec!["tools.old".to_string()]);
    state.clear(Some("config.toml"));
    assert!(state.toml_keys_for("config.toml").is_empty());
    assert_eq!(
        state.toml_keys_for("tools.toml"),
        vec!["tools.old".to_string()]
    );
}

#[test]
fn clear_all_drops_toml_keys() {
    let mut state = PrunedState::default();
    state.record_toml_keys("config.toml", vec!["providers.zai".to_string()]);
    state.clear(None);
    assert!(state.toml_keys.is_empty());
}

#[test]
fn save_and_parse_round_trip_toml_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("pruned.toml");
    let mut state = PrunedState::default();
    state.record_toml_keys(
        "config.toml",
        vec![
            "providers.zai".to_string(),
            "providers.custom.lm_studio".to_string(),
        ],
    );

    // `save()` writes to the real opencrabs home, so exercise the
    // serialisation through a direct write + parse at the same shape.
    let mut content = format!("schema_version = {SCHEMA_VERSION}\n\n[config.toml]\n");
    content.push_str("toml_keys = [\"providers.zai\", \"providers.custom.lm_studio\"]\n");
    std::fs::write(&path, &content).expect("write fixture");
    let loaded = PrunedState::parse(&std::fs::read_to_string(&path).expect("read fixture"));

    assert_eq!(
        loaded.toml_keys_for("config.toml"),
        state.toml_keys_for("config.toml")
    );
}
