//! Every Rust source file in the tree is either reachable from a `mod`
//! declaration or recorded here as deliberately unreachable (F-CH-01).
//!
//! Why this exists. A merge resolves a declaration conflict by taking one side,
//! and when the side it drops is the one that declared new files, those files
//! stay on disk and stop being compiled. Nothing fails: the tree still builds,
//! `cargo fmt` and `clippy` are clean, and the only symptom is code that nobody
//! runs. The 2026-10-15 upstream sync (`#263`) did exactly that to 42 test
//! module declarations and to the eight channel modules below, and the cleanup
//! (`#264`) cost a whole PR because the gap was invisible until someone read the
//! tree by hand.
//!
//! The class recurs on every sync, so the check has to be mechanical. Two lists
//! carry the exceptions, each entry naming WHY, and the test fails in both
//! directions: an undeclared file that is not on a list, and a listed file that
//! is now declared or gone. That second direction is what keeps the record from
//! rotting once the work lands.
//!
//! Scope: `src/`. Crate roots (`lib.rs`, `main.rs`) and the `Cargo.toml` target
//! paths are entry points, not modules.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The channel modules the sync deposited and the fork does not compile.
///
/// Each is byte-identical to its `upstream/main` copy, and each depends on the
/// upstream channel turn engine the fork holds back: `discord/turns.rs` needs
/// `DiscordState::active_turns`, `discord/tracked_turn.rs` needs
/// `channels::background_work::FlowOutcome`, `whatsapp/flow_message.rs` needs
/// `whatsapp::followups::send_suggestions`. Declaring them is therefore not a
/// one-line change, and declaring an unused module is refused outright: CI runs
/// `clippy -- -D warnings`, so `dead_code` fails the build.
///
/// The port is the price of holding the channel layer back, and it is a feature
/// job rather than a fix. Until it happens, these files are recorded here so the
/// gap is explicit and cannot grow silently.
const HELD_BACK_CHANNEL_MODULES: &[&str] = &[
    "src/channels/discord/options_host.rs",
    "src/channels/discord/tracked_turn.rs",
    "src/channels/discord/turns.rs",
    "src/channels/slack/flow_progress.rs",
    "src/channels/slack/options_host.rs",
    "src/channels/slack/resume_turn.rs",
    "src/channels/whatsapp/flow_message.rs",
    "src/channels/whatsapp/flow_render.rs",
];

/// Test modules the sync left undeclared and `#264` deliberately did not wire.
///
/// `#264` restored the 23 it could verify statically and recorded a reason for
/// each group it deferred: the channel-dependent ones exercise the fork's own
/// turn engine (which differs by design), four are commented out upstream and
/// stale against the current `PlanTask`, three import symbols the fork no longer
/// has, and one asserts a source shape the fork's sources do not have.
const DEFERRED_TEST_MODULES: &[&str] = &[
    "src/tests/discord_cowork_gate_test.rs",
    "src/tests/discord_dm_channel_history_test.rs",
    "src/tests/discord_flow_outcome_test.rs",
    "src/tests/discord_options_host_test.rs",
    "src/tests/discord_plan_board_test.rs",
    "src/tests/discord_resume_turn_tracking_test.rs",
    "src/tests/discord_turn_slot_test.rs",
    "src/tests/error_scenarios_test.rs",
    "src/tests/generate_image_multi_input_test.rs",
    "src/tests/integration_test.rs",
    "src/tests/models_session_pair_test.rs",
    "src/tests/onboarding_welcome_send_test.rs",
    "src/tests/plan_mode_integration_test.rs",
    "src/tests/slack_options_host_test.rs",
    "src/tests/slack_resume_turn_test.rs",
    "src/tests/streaming_test.rs",
    "src/tests/telegram_apply_all_confirm_test.rs",
    "src/tests/telegram_cli_models_menu_test.rs",
    "src/tests/whatsapp_composing_parity_test.rs",
    "src/tests/whatsapp_flow_options_test.rs",
    "src/tests/whatsapp_flow_render_test.rs",
];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `.rs` file under `src/`, as a repo-relative path with forward slashes.
fn source_files() -> BTreeSet<String> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeSet<String>) {
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let rel = path.strip_prefix(base).expect("under base");
                out.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let base = crate_root();
    let mut out = BTreeSet::new();
    walk(&base.join("src"), &base, &mut out);
    out
}

/// The directory a `mod x;` declared inside `file` resolves against.
///
/// rustc's rule, which the fork's layout exercises in both forms: a declaration
/// in `X/mod.rs` (or a crate root) looks in `X/`, while one in `X/name.rs` looks
/// in `X/name/`. `src/config/types.rs` declaring `mod io;` for
/// `src/config/types/io.rs` is the second form, and a walk that only reads
/// `mod.rs` reports those files as orphans.
fn module_dir(file: &str) -> String {
    let (dir, base) = match file.rsplit_once('/') {
        Some((d, b)) => (d.to_string(), b.to_string()),
        None => (String::new(), file.to_string()),
    };
    let stem = base.strip_suffix(".rs").unwrap_or(&base);
    if matches!(stem, "mod" | "lib" | "main") {
        dir
    } else if dir.is_empty() {
        stem.to_string()
    } else {
        format!("{dir}/{stem}")
    }
}

/// The module names `file` declares on one line each, raw identifiers stripped.
///
/// A declaration is read only when the line is a `mod NAME;` after any
/// visibility. An inline `mod NAME {` opens a body rather than a file and is
/// skipped, which is why the trailing semicolon is required.
fn declarations(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        if line.starts_with("//") || !line.contains("mod ") {
            continue;
        }
        let rest = line
            .strip_prefix("pub(crate) ")
            .or_else(|| line.strip_prefix("pub(super) "))
            .or_else(|| line.strip_prefix("pub "))
            .unwrap_or(line);
        let Some(rest) = rest.strip_prefix("mod ") else {
            continue;
        };
        let Some(name) = rest.strip_suffix(';') else {
            continue;
        };
        let name = name.trim().trim_start_matches("r#");
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            out.push(name.to_string());
        }
    }
    out
}

/// The `path = "src/…"` entries in `Cargo.toml`: entry points, not modules.
fn cargo_targets() -> BTreeSet<String> {
    let manifest =
        std::fs::read_to_string(crate_root().join("Cargo.toml")).expect("read Cargo.toml");
    let mut out = BTreeSet::new();
    for (i, _) in manifest.match_indices("path = \"src/") {
        let rest = &manifest[i + "path = \"".len()..];
        if let Some(end) = rest.find('"') {
            out.insert(rest[..end].to_string());
        }
    }
    out
}

fn entry_points() -> BTreeSet<String> {
    let mut out = cargo_targets();
    out.insert("src/lib.rs".to_string());
    out.insert("src/main.rs".to_string());
    out
}

/// The repo-relative paths that no `mod` declaration reaches.
fn undeclared(files: &BTreeSet<String>) -> BTreeSet<String> {
    let base = crate_root();
    let mut resolved: BTreeSet<String> = BTreeSet::new();
    for file in files {
        let src =
            std::fs::read_to_string(base.join(file)).unwrap_or_else(|e| panic!("read {file}: {e}"));
        // A module path attribute moves the file a declaration points at. The
        // fork uses none, and the walk would silently mis-resolve if one
        // appeared, so refuse rather than guess. The needle is assembled so this
        // file does not itself contain it.
        let attr = concat!("#[", "path");
        assert!(
            !src.contains(attr),
            "{file} carries a module path attribute; the module walk needs teaching before it \
             can be trusted"
        );
        let dir = module_dir(file);
        for name in declarations(&src) {
            for candidate in [format!("{dir}/{name}.rs"), format!("{dir}/{name}/mod.rs")] {
                if files.contains(&candidate) {
                    resolved.insert(candidate);
                }
            }
        }
    }
    files
        .difference(&resolved)
        .filter(|f| !entry_points().contains(*f))
        .cloned()
        .collect()
}

#[test]
fn every_source_file_is_declared_or_recorded_as_deliberately_unreachable() {
    let files = source_files();
    // Fixture sanity: the walk has to see the real tree, or every assertion
    // below is vacuous. The count only moves as files are added and removed.
    assert!(
        files.len() > 1_900,
        "the walk found {} .rs files, which is too few for this tree; the traversal is \
         broken rather than the tree being small",
        files.len()
    );

    let orphans = undeclared(&files);
    let recorded: BTreeSet<String> = HELD_BACK_CHANNEL_MODULES
        .iter()
        .chain(DEFERRED_TEST_MODULES.iter())
        .map(|s| s.to_string())
        .collect();

    let unrecorded: Vec<&String> = orphans.difference(&recorded).collect();
    assert!(
        unrecorded.is_empty(),
        "these source files are reachable from no `mod` declaration and are not recorded as \
         deliberately unreachable (F-CH-01). Declare each one, or add it to the list it \
         belongs to with the reason: {unrecorded:#?}"
    );

    let stale: Vec<&String> = recorded.difference(&orphans).collect();
    assert!(
        stale.is_empty(),
        "these files are listed as unreachable but are no longer undeclared (or are gone), so \
         the record has rotted. Remove them from the list: {stale:#?}"
    );
}

/// The detector, exercised on the shapes it has to tell apart: the two module
/// directory forms, a raw identifier, a declaration the walk must NOT count, and
/// a plain file that no declaration names.
#[test]
fn the_module_walk_resolves_both_declaration_forms() {
    // `X/mod.rs` declaring `mod a;` resolves to `X/a.rs`.
    assert_eq!(
        module_dir("src/channels/slack/mod.rs"),
        "src/channels/slack"
    );
    // `X/name.rs` declaring `mod a;` resolves to `X/name/a.rs`.
    assert_eq!(module_dir("src/config/types.rs"), "src/config/types");
    // A crate root is the directory it sits in.
    assert_eq!(module_dir("src/lib.rs"), "src");

    assert_eq!(declarations("mod a;\n"), vec!["a"]);
    assert_eq!(declarations("pub(crate) mod b;\n"), vec!["b"]);
    assert_eq!(declarations("pub mod c;\n"), vec!["c"]);
    // A raw identifier names `trait.rs`, not `r#trait.rs`.
    assert_eq!(declarations("pub(crate) mod r#trait;\n"), vec!["trait"]);
    // An inline module opens a body and names no file.
    assert!(declarations("mod inline {\n    fn f() {}\n}\n").is_empty());
    // A mention in prose is not a declaration.
    assert!(declarations("// see mod foo; for the details\n").is_empty());
}
