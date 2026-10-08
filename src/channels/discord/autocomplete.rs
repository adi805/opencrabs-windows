//! FR-005 (AC-007, AC-008): typing-time suggestions for command options whose
//! values come from a live catalog.
//!
//! Discord asks for suggestions while the user is still typing (interaction
//! type 4) and expects an answer inside the same three-second window as any
//! other interaction. Two consequences shape everything in this file:
//!
//! - The filter is a pure function over an already-gathered snapshot, so the
//!   decision is testable without a gateway, a config file, or a clock.
//! - A request that cannot be answered usefully answers with an EMPTY list
//!   instead of an error. A command this build does not enumerate, an option
//!   that is not the enumerable one, and a catalog that came back empty all get
//!   the same answer: Discord renders an empty list as "no suggestions", while
//!   a failed interaction puts an error in front of the user for a keystroke
//!   (AC-008).
//!
//! The catalogs are read live on every request: providers and models from the
//! running config, sessions from the session store. Nothing here holds a static
//! list of values, which is the difference AC-007 is written to catch.

use std::sync::Arc;

use serenity::builder::{
    AutocompleteChoice, CreateAutocompleteResponse, CreateInteractionResponse,
};
use serenity::http::Http;
use serenity::model::application::CommandInteraction;

use crate::config::Config;
use crate::db::repository::SessionListOptions;
use crate::services::SessionService;

/// The most choices Discord accepts on one autocomplete response.
pub(crate) const MAX_CHOICES: usize = 25;

/// Discord rejects a choice whose name runs past this many characters.
pub(crate) const CHOICE_MAX: usize = 100;

/// How many stored sessions the snapshot reads before filtering.
///
/// The filter runs here rather than in SQL so there is exactly one matching
/// rule; the cost is that the window has to be wider than one page of
/// suggestions, or a session far down the list could never be offered.
const SESSION_SCAN: usize = 200;

/// A live catalog an enumerable option draws its values from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Catalog {
    /// Ids of the providers this config has configured.
    Providers,
    /// Models those providers offer.
    Models,
    /// Titles of stored sessions.
    Sessions,
}

/// The catalog behind a registered command name, or `None` for free text.
///
/// Matched on the name Discord sends back, which is the sanitized one
/// (`commands::sanitize_name`: lowercase, no leading slash). The leading slash
/// is stripped here too, so the table also reads naturally against a
/// `commands.toml` entry, where the name keeps it.
///
/// This is deliberately a table rather than a config field. Every command is
/// registered with the same single `args` string, so nothing in the
/// registration distinguishes "pick a model" from "paste an argument", and the
/// command name is the only signal available at interaction time. Adding
/// `/models` or `/sessions` to `commands.toml` lights up its suggestions with
/// no code change here.
pub(crate) fn catalog_for(command: &str) -> Option<Catalog> {
    let name = command.trim().trim_start_matches('/').to_lowercase();
    match name.as_str() {
        "providers" | "provider" => Some(Catalog::Providers),
        "models" | "model" => Some(Catalog::Models),
        "sessions" | "session" | "resume" => Some(Catalog::Sessions),
        _ => None,
    }
}

/// Every catalog, gathered once per request.
///
/// One struct rather than three slices so a caller holding only the cheap
/// config half can leave the session list empty without changing any
/// signature, and so the pure filter takes one argument instead of three.
#[derive(Debug, Clone, Default)]
pub(crate) struct CatalogValues {
    /// Provider ids, in `configured_providers` order.
    pub(crate) providers: Vec<String>,
    /// Models, first mention wins.
    pub(crate) models: Vec<String>,
    /// Session titles, newest first.
    pub(crate) sessions: Vec<String>,
}

impl CatalogValues {
    /// The values behind `catalog`.
    pub(crate) fn get(&self, catalog: Catalog) -> &[String] {
        match catalog {
            Catalog::Providers => &self.providers,
            Catalog::Models => &self.models,
            Catalog::Sessions => &self.sessions,
        }
    }
}

/// The suggestions for one autocomplete request.
///
/// The single pure entry point for both halves of FR-005: a command with a
/// catalog filters that catalog, and everything else gets an empty list rather
/// than an error (AC-008). Keeping the negative path in the same function as
/// the positive one is what makes "never errors" checkable instead of
/// aspirational.
pub(crate) fn suggestions_for(command: &str, typed: &str, values: &CatalogValues) -> Vec<String> {
    match catalog_for(command) {
        Some(catalog) => suggest(values.get(catalog), typed),
        None => Vec::new(),
    }
}

/// Filter `values` down to the suggestions for `typed`.
///
/// Case-insensitive substring match, with the values that START with the typed
/// text first, so a user who has typed enough for an exact prefix sees the
/// completion before the coincidences. Order inside each group is the order the
/// catalog supplied, which makes the result deterministic for a given catalog
/// and keeps the newest session first.
///
/// An empty `typed` returns the head of the catalog: Discord sends an empty
/// value on focus, and showing what is available before anything is typed is
/// the point of the interaction.
pub(crate) fn suggest(values: &[String], typed: &str) -> Vec<String> {
    let needle = typed.trim().to_lowercase();
    let mut prefixed: Vec<String> = Vec::new();
    let mut contained: Vec<String> = Vec::new();

    for value in values {
        if value.trim().is_empty() {
            continue;
        }
        let folded = value.to_lowercase();
        if !folded.contains(&needle) {
            continue;
        }
        let bucket = if folded.starts_with(&needle) {
            &mut prefixed
        } else {
            &mut contained
        };
        if !bucket.iter().any(|kept| kept == value) {
            bucket.push(value.clone());
        }
    }

    prefixed.extend(contained);
    prefixed.truncate(MAX_CHOICES);
    prefixed.into_iter().map(clip).collect()
}

/// Clamp one suggestion to Discord's choice-name budget, counting characters
/// rather than bytes so a multi-byte title is never cut mid-codepoint.
fn clip(value: String) -> String {
    if value.chars().count() <= CHOICE_MAX {
        value
    } else {
        value.chars().take(CHOICE_MAX).collect()
    }
}

/// Answer an autocomplete interaction: gather the catalog, filter it, reply.
///
/// Every path ends in a response or a log line. The window is three seconds and
/// Discord shows the user a failure for anything it does not get an answer to,
/// so a refused response is logged and dropped rather than propagated: there is
/// nothing useful to do with it, and the caller is an event loop that must not
/// be derailed by a keystroke. An expired interaction token lands in exactly
/// that branch, which is why the negative path needs no code of its own
/// (AC-008).
pub(crate) async fn answer(
    http: &Arc<Http>,
    command: &CommandInteraction,
    cfg: &Config,
    sessions: &SessionService,
) {
    let typed = command.data.autocomplete().map(|opt| opt.value.to_string());
    let choices = match typed {
        Some(typed) => {
            let values = match catalog_for(&command.data.name) {
                Some(catalog) => gather(catalog, cfg, sessions).await,
                None => CatalogValues::default(),
            };
            suggestions_for(&command.data.name, &typed, &values)
        }
        None => Vec::new(),
    };

    let payload: Vec<AutocompleteChoice> = choices
        .into_iter()
        .map(|value| AutocompleteChoice::new(value.clone(), value))
        .collect();
    let response = CreateInteractionResponse::Autocomplete(
        CreateAutocompleteResponse::new().set_choices(payload),
    );
    if let Err(e) = command.create_response(http, response).await {
        tracing::warn!(
            "Discord: autocomplete response for /{} failed: {e}",
            command.data.name
        );
    }
}

/// Read one catalog from its live source.
///
/// Only the requested catalog is read: providers and models come out of the
/// in-memory config, while sessions cost a query, and the three-second window
/// is not a budget to spend on values nobody asked for.
async fn gather(catalog: Catalog, cfg: &Config, sessions: &SessionService) -> CatalogValues {
    match catalog {
        Catalog::Providers => CatalogValues {
            providers: provider_ids(cfg),
            ..CatalogValues::default()
        },
        Catalog::Models => CatalogValues {
            models: configured_models(cfg),
            ..CatalogValues::default()
        },
        Catalog::Sessions => CatalogValues {
            sessions: session_titles(sessions).await,
            ..CatalogValues::default()
        },
    }
}

/// Ids of every provider this config has configured.
fn provider_ids(cfg: &Config) -> Vec<String> {
    crate::utils::providers::configured_providers(&cfg.providers)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// Every model the configured providers offer, first mention wins.
fn configured_models(cfg: &Config) -> Vec<String> {
    let mut models: Vec<String> = Vec::new();
    for (id, _) in crate::utils::providers::configured_providers(&cfg.providers) {
        let Some(section) = crate::utils::providers::config_for(&cfg.providers, &id) else {
            continue;
        };
        for model in &section.models {
            if !models.contains(model) {
                models.push(model.clone());
            }
        }
    }
    models
}

/// Titles of the stored sessions, newest first.
///
/// A store that cannot be read yields an empty list rather than an error: an
/// unreachable database is a reason to offer no suggestions, not a reason to
/// fail a keystroke (AC-008).
async fn session_titles(sessions: &SessionService) -> Vec<String> {
    let options = SessionListOptions {
        include_archived: false,
        limit: Some(SESSION_SCAN),
        offset: 0,
        query: None,
        include_subagents: false,
    };
    sessions
        .list_sessions(options)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|session| session.title)
        .filter(|title| !title.trim().is_empty())
        .collect()
}
