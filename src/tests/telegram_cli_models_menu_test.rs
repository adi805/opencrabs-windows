//! #2041: the CLI /models menu shows the live list, and Current is the
//! configured default rather than the first entry.

// ── #2041: the CLI /models menu ────────────────────────────────────────

use crate::channels::commands::cli_menu_choice;

#[test]
fn current_is_the_configured_default_not_the_first_list_entry() {
    let discovered = vec!["fable-5-1".to_string(), "haiku-5-5".to_string()];
    let (current, models) =
        cli_menu_choice(discovered.clone(), "sonnet", Some("haiku-5-5".to_string()));
    assert_eq!(current, "haiku-5-5");
    assert_eq!(models, discovered);
}

#[test]
fn current_falls_back_to_the_canonical_default_without_a_configured_one() {
    let (current, _) = cli_menu_choice(vec!["fable".to_string()], "sonnet", None);
    assert_eq!(current, "sonnet");
}

#[test]
fn the_list_is_the_discovered_one_whatever_was_saved() {
    // The function takes no saved list: a frozen config array cannot reach it.
    let discovered: Vec<String> = ["fable-5-1", "haiku-5-5", "opus-5-5"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (_, models) = cli_menu_choice(discovered.clone(), "sonnet", None);
    assert_eq!(models, discovered);
}
