//! Every execution tool honors the bash hard blocklist (OC-06, OC-A2).
//!
//! `bash` documents `check_blocked_command` as the immovable floor that applies
//! even under approval, but `execute_code`, dynamic shell executors, and cron
//! ran their commands without it, so `rm -rf ~` (and fork bombs, /etc/shadow
//! reads, ...) through those tools was a complete bypass. `assert_command_allowed`
//! is the single floor they now share.
//!
//! The list below used to be written by hand, and that is exactly how
//! `cron/trigger.rs` and `plan_tool.rs` kept running shell commands with no
//! floor while this file's own doc comment already claimed cron was covered: a
//! hand list only checks the files somebody remembered. The set of spawn sites
//! is now ENUMERATED by walking `src/`, and every file that spawns a shell must
//! be classified, so a new unfloored path fails here instead of shipping.

use crate::brain::tools::bash::assert_command_allowed;
use std::path::Path;

#[test]
fn the_floor_refuses_a_destructive_command() {
    // Reuse the shapes already in the bash blocklist rather than inventing new
    // destructive strings.
    assert!(assert_command_allowed("rm -rf ~").is_some());
    assert!(assert_command_allowed("rm -rf /").is_some());
}

#[test]
fn the_floor_allows_an_ordinary_command() {
    assert!(assert_command_allowed("echo hello").is_none());
    assert!(assert_command_allowed("ls -la /tmp").is_none());
}

// ── which files spawn a platform shell ──────────────────────────────────────

/// Textual signature of a platform-shell spawn: the argv pair plus the trait
/// method that pushes the command line onto it.
fn spawns_a_shell(text: &str) -> bool {
    text.contains("shell_pair()") || text.contains("push_shell_command(")
}

/// Every `.rs` file under `dir`, recursively, skipping `dir/tests`.
///
/// `src/tests` is the unit-test tree; test files are allowed to shell out
/// without the floor.
fn rust_sources(dir: &Path, out: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{dir:?}: {e}"));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "tests") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn spawn_sites() -> Vec<String> {
    let mut all = Vec::new();
    rust_sources(Path::new("src"), &mut all);
    all.sort();
    all.retain(|rel| {
        std::fs::read_to_string(rel)
            .map(|text| spawns_a_shell(&text))
            .unwrap_or(false)
    });
    all
}

/// Every execution path that must call the shared floor.
///
/// All but `code_exec` spawn a platform shell, so the walk below finds them.
/// `code_exec` spawns the interpreter directly and is floored without
/// appearing in the spawn-site scan.
const FLOORED: &[&str] = &[
    "src/brain/tools/bash.rs",
    "src/brain/tools/code_exec.rs",
    "src/brain/tools/dynamic/tool.rs",
    "src/brain/tools/plan_tool.rs",
    "src/cron/trigger.rs",
];

/// Shell spawn sites the scan MUST find, whatever else it finds.
///
/// Guards the scanner: a predicate that matched nothing would make the
/// classification test pass vacuously, which is the failure mode this file
/// exists to prevent.
const MUST_BE_FOUND: &[&str] = &[
    "src/brain/tools/bash.rs",
    "src/brain/tools/dynamic/tool.rs",
    "src/cron/trigger.rs",
];

/// Spawn sites deliberately outside the floor, each with its reason.
///
/// A spawn site belongs here only when the command is not agent-chosen, or when
/// the floor has already run upstream of it.
const UNFLOORED_BY_DESIGN: &[(&str, &str)] = &[
    (
        "src/utils/shell.rs",
        "the shell-argv helper itself: it builds the command line, it does not decide what runs",
    ),
    (
        "src/brain/agent/service/background_tasks.rs",
        "only reached from bash.rs, after check_blocked_command has already run on the command",
    ),
    (
        "src/tui/app/input.rs",
        "the TUI `!command` escape, typed by the human at the terminal, not chosen by the agent",
    ),
];

#[test]
fn the_walk_finds_the_spawn_sites() {
    let found = spawn_sites();
    for expected in MUST_BE_FOUND {
        assert!(
            found.iter().any(|f| f == expected),
            "the scanner did not find {expected}; the spawn-site predicate is broken, so the \
             classification test proves nothing (found: {found:?})"
        );
    }
}

#[test]
fn every_shell_spawn_site_is_classified() {
    let known: Vec<&str> = FLOORED
        .iter()
        .copied()
        .chain(UNFLOORED_BY_DESIGN.iter().map(|(rel, _)| *rel))
        .collect();
    for rel in spawn_sites() {
        assert!(
            known.contains(&rel.as_str()),
            "{rel} spawns a platform shell but is not classified here. Either call \
             assert_command_allowed on the command (OC-06) and add it to FLOORED, or add it to \
             UNFLOORED_BY_DESIGN with the reason it cannot be agent-chosen."
        );
    }
}

#[test]
fn every_floored_site_calls_the_shared_floor() {
    for rel in FLOORED {
        let text = std::fs::read_to_string(Path::new(rel))
            .unwrap_or_else(|e| panic!("{rel} must be readable ({e})"));
        assert!(
            text.contains("assert_command_allowed"),
            "{rel} runs commands but does not call assert_command_allowed, so it can bypass the \
             bash blocklist (OC-06)"
        );
    }
}

#[test]
fn every_by_design_exclusion_states_a_reason() {
    for (rel, reason) in UNFLOORED_BY_DESIGN {
        assert!(
            !reason.trim().is_empty(),
            "{rel} is excluded from the floor with no reason given"
        );
    }
}

#[test]
fn a_dynamic_tool_cannot_reserve_a_core_tool_name() {
    // The loader must refuse a dynamic tool that would shadow a core one.
    let loader = std::fs::read_to_string("src/brain/tools/dynamic/loader.rs").unwrap();
    assert!(
        loader.contains("reserved core tool name") && loader.contains("\"bash\""),
        "add_tool must reserve core tool names so a dynamic 'bash' cannot replace the real one"
    );
}
