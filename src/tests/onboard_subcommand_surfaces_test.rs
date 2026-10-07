//! The setup command set must read the same on every surface (#1664, #1981).
//!
//! `README.md` once documented `/onboard:gateway` for a step that did not
//! exist and an arm that was never written. Typing it opened the full wizard
//! at the mode selector, silently, because the dispatch match ended in a
//! catch-all that swallowed every unrecognised suffix. A stale doc row was
//! therefore indistinguishable from bare `/onboard`, and nothing failed until
//! a user followed the README.
//!
//! Dispatch is the source of truth. Since #1981 the offered set is the direct
//! commands (`SETUP_COMMANDS`); the legacy `/onboard:<step>` spellings still
//! resolve but must not be offered anywhere. These tests hold the README
//! table and the slash registry to dispatch in both directions.

use crate::tui::app::state::SLASH_COMMANDS;
use crate::tui::onboarding::deep_link::{
    LEGACY_ONBOARD_SUBCOMMANDS, SETUP_COMMANDS, is_setup_command, unknown_suffix_message,
};
use std::fs;
use std::path::Path;

fn readme() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("README.md must be readable")
}

/// The command in every `| `/x ...` |` row of the README command tables,
/// first word only (`/channels [name]` -> `/channels`).
fn readme_commands(readme: &str) -> Vec<String> {
    readme
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("| `/")?;
            let cell = rest.split('`').next()?;
            let word = cell.split_whitespace().next()?;
            Some(format!("/{word}"))
        })
        .collect()
}

#[test]
fn every_setup_command_is_documented_in_the_readme() {
    let rows = readme_commands(&readme());
    for (name, _) in SETUP_COMMANDS {
        assert!(
            rows.iter().any(|r| r == name),
            "{name} dispatches to a step but no README row mentions it"
        );
    }
}

#[test]
fn every_setup_command_is_offered_by_autocomplete() {
    for (name, _) in SETUP_COMMANDS {
        assert!(
            SLASH_COMMANDS.iter().any(|c| c.name == *name),
            "{name} dispatches to a step but is absent from SLASH_COMMANDS, \
             so neither autocomplete nor the help dialog can offer it"
        );
    }
}

#[test]
fn legacy_onboard_spellings_are_offered_nowhere() {
    assert!(
        !SLASH_COMMANDS
            .iter()
            .any(|c| c.name.starts_with("/onboard:")),
        "autocomplete must offer the direct commands, not /onboard:<step>"
    );
    let rows = readme_commands(&readme());
    assert!(
        !rows.iter().any(|r| r.starts_with("/onboard:")),
        "README command tables must list the direct commands, not /onboard:<step>"
    );
}

#[test]
fn every_documented_setup_row_has_a_dispatch_arm() {
    // A README row naming a setup step must be a real command: a direct one,
    // or bare /onboard.
    let setup_words = ["workspace", "channels", "voice", "image", "daemon", "brain"];
    for row in readme_commands(&readme()) {
        let word = row.trim_start_matches('/');
        if setup_words.contains(&word) {
            assert!(is_setup_command(&row), "README documents {row} with no arm");
        }
    }
}

#[test]
fn gateway_is_gone_from_every_surface() {
    assert!(
        !readme().contains("/onboard:gateway"),
        "there is no gateway wizard step and no gateway dispatch arm"
    );
    assert!(
        !LEGACY_ONBOARD_SUBCOMMANDS
            .iter()
            .any(|(n, _)| *n == "gateway")
    );
    assert!(!is_setup_command("/gateway"));
}

#[test]
fn unknown_suffix_message_names_the_offender_and_the_direct_commands() {
    let msg = unknown_suffix_message("gateway");
    assert!(msg.contains("gateway"), "{msg}");
    for (name, _) in SETUP_COMMANDS {
        assert!(msg.contains(name), "valid set must list {name}: {msg}");
    }
    assert!(
        !msg.contains("/onboard:"),
        "must not steer to legacy spellings: {msg}"
    );
}
