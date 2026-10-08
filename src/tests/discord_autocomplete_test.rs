//! FR-005 / AC-007, AC-008: suggestions for command options whose values come
//! from a live catalog.
//!
//! Two halves, tested the two ways the repo already tests this kind of thing.
//!
//! The decision itself is pure (`suggest`, `suggestions_for`), so it is tested
//! directly: filtering, ranking, the 25-choice cap, the 100-character name
//! budget, and — the part AC-008 is written for — that every negative path
//! produces an EMPTY LIST rather than an error.
//!
//! What a pure test cannot reach is the wiring, and the wiring is where this
//! feature actually fails: an option that never sets `autocomplete` gets no
//! request from Discord at all, and an unhandled `Interaction::Autocomplete`
//! turns every keystroke into a timeout the user sees as an error. So the last
//! two tests read `agent.rs` and `commands.rs` and pin that shape, the same way
//! `discord_auto_thread_test` pins the send path.
//!
//! Scope, stated so a later reader does not widen it: this covers the option
//! Discord calls `args` on the Discord channel. It says nothing about Telegram's
//! menu or the TUI's completion, which are separate projections of the same
//! catalog.

use std::path::Path;

use crate::channels::discord::autocomplete::{
    CHOICE_MAX, Catalog, CatalogValues, MAX_CHOICES, catalog_for, suggestions_for,
};

/// The named source file, read from the crate root.
fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The same source with all whitespace removed, so a call split across lines
/// still matches a needle written on one line.
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

// ---------------------------------------------------------------------------
// The decision (AC-007)
// ---------------------------------------------------------------------------

/// AC-007: the values offered are the ones the running config holds, and a
/// different command offers a different list.
#[test]
fn suggestions_come_from_the_live_catalog_not_a_static_list() {
    let providers = CatalogValues {
        providers: vec![
            "inferhub".to_string(),
            "commandgoat".to_string(),
            "zai-coding".to_string(),
        ],
        ..CatalogValues::default()
    };

    // Discord sends an empty value on focus: showing what is available before
    // anything is typed is the whole point of the interaction.
    assert_eq!(
        suggestions_for("/providers", "", &providers),
        vec!["inferhub", "commandgoat", "zai-coding"],
        "focusing the option must offer the configured providers"
    );
    assert_eq!(
        suggestions_for("/providers", "infer", &providers),
        vec!["inferhub"],
        "a partial value must narrow to what the config actually holds"
    );

    let models = CatalogValues {
        models: vec![
            "combo/workhorse".to_string(),
            "ag/gemini-3.7-flash-high".to_string(),
        ],
        ..CatalogValues::default()
    };
    assert_eq!(
        suggestions_for("/models", "workhorse", &models),
        vec!["combo/workhorse"]
    );
    assert!(
        suggestions_for("/models", "workhorse", &providers).is_empty(),
        "a provider id is not a model: the catalogs must not be mixed"
    );
}

/// A completion is more useful than a coincidence, so it ranks first; order
/// inside a bucket stays catalog order so the result is deterministic.
#[test]
fn prefix_matches_rank_before_substring_matches() {
    let values = CatalogValues {
        sessions: vec![
            "fix the inferhub timeout".to_string(),
            "inferhub routing".to_string(),
            "inferhub routing".to_string(),
        ],
        ..CatalogValues::default()
    };
    assert_eq!(
        suggestions_for("/sessions", "inferhub", &values),
        vec!["inferhub routing", "fix the inferhub timeout"],
        "the completion comes before the coincidence, and the duplicate is \
         collapsed so two identical buttons never render"
    );
}

/// Discord rejects a response with more than 25 choices, which would surface to
/// the user as a failed interaction.
#[test]
fn the_suggestion_list_is_capped_at_discords_limit() {
    let many: Vec<String> = (0..40).map(|i| format!("provider-{i:02}")).collect();
    let values = CatalogValues {
        providers: many,
        ..CatalogValues::default()
    };
    let got = suggestions_for("/providers", "", &values);
    assert_eq!(got.len(), MAX_CHOICES, "at most 25 choices per response");
    assert_eq!(got[0], "provider-00");
    assert_eq!(got[MAX_CHOICES - 1], "provider-24");
}

/// A choice name is capped at 100 characters. Clipped by CHARACTERS: a byte
/// slice here would split a multi-byte codepoint and produce invalid UTF-8,
/// which Discord would reject for the whole response.
#[test]
fn a_choice_never_exceeds_discords_name_budget() {
    let values = CatalogValues {
        models: vec!["é".repeat(150)],
        ..CatalogValues::default()
    };
    let got = suggestions_for("/models", "", &values);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].chars().count(), CHOICE_MAX);
    assert_eq!(got[0], "é".repeat(CHOICE_MAX));
}

// ---------------------------------------------------------------------------
// The negative path (AC-008)
// ---------------------------------------------------------------------------

/// AC-008: a command with no catalog offers nothing, and that is a valid answer
/// rather than an error. Every one of these names is a real command in
/// `commands.toml` except the last two.
#[test]
fn an_unknown_command_gets_an_empty_list_instead_of_an_error() {
    let values = CatalogValues {
        providers: vec!["inferhub".to_string()],
        models: vec!["combo/workhorse".to_string()],
        sessions: vec!["some session".to_string()],
    };
    for name in [
        "/health",
        "/restart",
        "/audit",
        "/menu",
        "/status",
        "not-a-command",
        "",
    ] {
        assert!(
            suggestions_for(name, "inf", &values).is_empty(),
            "{name:?} has no catalog, so it must offer nothing rather than \
             filter some other command's values (AC-008)"
        );
    }
}

/// AC-008's other half: when the catalog cannot be produced — an expired
/// interaction token, an unreachable session store, a config with nothing
/// configured — the answer is still an empty list. This is the stale-token
/// case, and it is why the gather step resolves to `CatalogValues::default()`
/// rather than propagating a failure.
#[test]
fn a_stale_or_empty_catalog_gets_an_empty_list_instead_of_an_error() {
    let none = CatalogValues::default();
    for name in ["/sessions", "/providers", "/models"] {
        assert!(
            suggestions_for(name, "anything", &none).is_empty(),
            "{name} with no readable catalog must answer empty, not error"
        );
    }
}

/// The registration side and the answer side read the same name, and the
/// catalog file writes it with a leading slash while Discord sends it without.
#[test]
fn the_command_table_reads_a_slash_and_a_bare_name_the_same_way() {
    assert_eq!(catalog_for("/providers"), Some(Catalog::Providers));
    assert_eq!(catalog_for("providers"), Some(Catalog::Providers));
    assert_eq!(catalog_for("/PROVIDERS"), Some(Catalog::Providers));
    assert_eq!(catalog_for("/model"), Some(Catalog::Models));
    assert_eq!(catalog_for("/resume"), Some(Catalog::Sessions));
    assert_eq!(catalog_for("/health"), None);
    assert_eq!(catalog_for(""), None);
}

// ---------------------------------------------------------------------------
// The wiring (AC-007)
// ---------------------------------------------------------------------------

/// AC-007: the gateway answers autocomplete, and it answers it before the
/// command path. Both halves matter: with no arm every request times out and
/// Discord shows the user an error; behind the command arm the answer shares a
/// three-second window with a spawned turn.
#[test]
fn the_interaction_arm_answers_autocomplete() {
    let src = read("src/channels/discord/agent.rs");
    let flat_src = flat(&src);

    assert!(
        flat_src.contains("ifletInteraction::Autocomplete(command)=&interaction{"),
        "the gateway has no autocomplete arm, so every request for suggestions \
         times out and Discord shows the user an error (AC-007)"
    );
    assert!(
        flat_src
            .contains("super::autocomplete::answer(&ctx.http,command,&cfg,&self.session_svc)"),
        "the arm no longer routes to the module that owns the decision, so the \
         answer can drift from the catalog it is supposed to read (AC-007)"
    );

    let arm = src
        .find("Interaction::Autocomplete(command) = &interaction")
        .expect("the autocomplete arm is gone (AC-007)");
    let command_arm = src
        .find("if let Interaction::Command(command) = &interaction")
        .expect("the command arm is gone (#1850)");
    assert!(
        arm < command_arm,
        "autocomplete is answered after the command path it shares a window \
         with: a suggestion must not queue behind a spawned turn (AC-007)"
    );
}

/// AC-007: the client only sends an autocomplete interaction for an option that
/// asked for one, so the flag has to be set at registration — and only where a
/// catalog exists, because a flag with no catalog is a dead interaction.
#[test]
fn only_enumerable_commands_ask_the_client_for_suggestions() {
    let src = read("src/channels/discord/commands.rs");
    let flat_src = flat(&src);

    assert!(
        flat_src.contains("super::autocomplete::catalog_for(source_name).is_some()"),
        "the option flag no longer consults the catalog table, so what is \
         registered and what is answered can drift (AC-007)"
    );
    assert!(
        flat_src.contains("option.set_autocomplete(true)"),
        "no option asks for autocomplete, so the client never sends an \
         autocomplete interaction and the arm is unreachable (AC-007)"
    );
    assert!(
        flat_src.contains(".add_option(command_option(&entry.name))"),
        "the command builder stopped using the shared option helper, so the \
         flag can be forgotten on one path (AC-007)"
    );
    assert_eq!(
        src.matches("CreateCommandOption::new(").count(),
        1,
        "a second option builder appeared inline: that is how the flag gets \
         forgotten on one path and kept on another (AC-007)"
    );
}
