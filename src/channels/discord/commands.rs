//! Native Discord application commands for the Discord channel (#1850).
//!
//! `commands.toml` is the single source of truth for user commands on every
//! channel: Telegram renders it as the BotFather menu (see
//! `src/channels/telegram/menu_refresh.rs`), the TUI completes it, and the
//! `slash_command` tool executes it. Discord had no projection at all, so the
//! commands existed and were invisible: there was no `/` autocomplete and no way
//! to invoke them except typing the name into a message.
//!
//! Scope note: `commands.toml` plus a fixed set of built-ins. Telegram's menu
//! additionally lists built-ins and skills; [`with_menu_builtins`] closes part of
//! that gap and carries only the built-ins the interaction path can answer
//! ([`MENU_BUILTINS`]). Skills are still not projected: they work when typed as
//! text, through the normal message path, and adding them is a loop over
//! `brain::skills::load_all_skills()` in [`sync_commands`] plus its own decision
//! about which skills belong in a guild menu.
//!
//! This module is the projection. It turns the catalog into CHAT_INPUT builders
//! and pushes them with the *bulk overwrite* route
//! (`Command::set_global_commands`, which is
//! `PUT /applications/{application.id}/commands`), so a sync replaces the whole
//! set instead of creating commands one by one. That distinction matters for
//! Discord's budgets: the 200-per-day-per-guild limit
//! (`developers/interactions/application-commands.mdx:208`) counts per-command
//! **creates**, which bulk overwrite does not perform.
//!
//! The route is global rather than guild-scoped because a guild-scoped command
//! is unreachable in a DM: Discord serves a DM's command list only from the
//! global set. The trade is propagation latency, up to an hour for a global
//! write against immediate for a guild-scoped one, which is worth paying once
//! because the guild menu this feature used to write is cleared on the same
//! sync (see [`sync_commands`]) instead of lingering as a shadowing duplicate.
//!
//! Everything here is pure except [`sync_commands`], which takes the HTTP handle
//! and a guild list. Splitting it that way is what makes the grammar rules
//! testable without a Discord application, because there is no bot in CI.
//!
//! ## Name and description grammar
//!
//! Discord rejects a command whose name is not `^[\w-]{1,32}$` (lowercase, at
//! most 32 characters) or whose description is longer than 100 characters, and
//! caps a guild at 100 CHAT_INPUT commands with 8000 characters across the whole
//! command tree. The catalog is free-form text, so every one of those is a real
//! failure mode rather than a theoretical one. The rules are applied in order:
//! sanitize, then dedupe by the sanitized name, then cap, then tree size.
//!
//! Dropping is the only sane option where a rule bites. A rejected name would
//! otherwise take the whole sync with it, including every command that was fine,
//! so the bad entry is dropped loudly and the rest of the guild keeps its menu.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serenity::builder::{CreateCommand, CreateCommandOption};
use serenity::http::Http;
use serenity::model::application::{
    Command, CommandDataOptionValue, CommandInteraction, CommandOptionType, CommandType,
};
use serenity::model::id::GuildId;

use crate::brain::{BrainLoader, CommandLoader, UserCommand};

/// Discord's `^[\w-]{1,32}$` name cap.
pub(crate) const NAME_MAX: usize = 32;

/// CHAT_INPUT descriptions cap at 100 characters.
pub(crate) const DESCRIPTION_MAX: usize = 100;

/// CHAT_INPUT commands per guild (`application-commands.mdx:201`).
pub(crate) const GUILD_COMMAND_CAP: usize = 100;

/// Character budget across one guild's command tree (`:481`).
pub(crate) const TREE_CHAR_CAP: usize = 8000;

/// Every user command is registered with exactly this one optional string
/// option. The argument text is not parsed here: the interaction arm rebuilds
/// `"/<name> <args>"` and feeds it to the same turn router a typed message
/// uses, so the `slash_command` tool sees what it would have seen from chat.
/// Parsing per-command in the channel would fork command semantics into two
/// implementations, and only Discord would behave that way.
pub(crate) const ARGS_OPTION: &str = "args";

/// Description shown for [`ARGS_OPTION`] in the client's argument UI.
pub(crate) const ARGS_OPTION_DESCRIPTION: &str = "Text to pass to the command";

/// Why an entry did not make it into the command set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DropReason {
    /// Nothing legal survived sanitizing, so there is no name to register.
    InvalidName,
    /// Another entry already holds this Discord name. The first one in catalog
    /// order wins, and `kept` names it.
    Collision {
        /// Catalog name that already owns the sanitized name.
        kept: String,
    },
    /// Past [`GUILD_COMMAND_CAP`]; catalog order decides who is kept.
    OverCap,
    /// Including this command would push the tree past [`TREE_CHAR_CAP`].
    TreeTooLarge,
}

/// One catalog entry that was left out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dropped {
    /// The name as written in the catalog.
    pub(crate) source: String,
    /// Why it did not make it in.
    pub(crate) reason: DropReason,
}

/// The command set a guild should end up with, plus what was excluded.
#[derive(Debug, Clone, Default)]
pub(crate) struct CommandPlan {
    /// Builders in guild order, ready for bulk overwrite.
    pub(crate) commands: Vec<CreateCommand>,
    /// `(registered name, catalog name, description)` parallel with
    /// [`CommandPlan::commands`]. Held because the builders do not expose their
    /// fields, so without this the plan could not be inspected or signed.
    pub(crate) entries: Vec<(String, String, String)>,
    /// Catalog entries left out, in catalog order.
    pub(crate) dropped: Vec<Dropped>,
}

impl CommandPlan {
    /// A signature that changes exactly when the registered set changes, and
    /// not when the catalog is merely reordered. Same shape as
    /// `menu_refresh::skills_signature`: identity over a sorted projection of
    /// the fields that end up on the wire.
    pub(crate) fn signature(&self) -> u64 {
        let mut items: Vec<(String, String)> = self
            .entries
            .iter()
            .map(|(n, _, d)| (n.clone(), d.clone()))
            .collect();
        items.sort();
        let mut hasher = DefaultHasher::new();
        for (name, description) in &items {
            name.hash(&mut hasher);
            description.hash(&mut hasher);
        }
        hasher.finish()
    }
}

/// Turn a catalog name into a Discord-legal command name: drop the leading
/// slash, lowercase it, map anything outside `[a-z0-9_-]` to `-`, trim edge
/// dashes and clamp to [`NAME_MAX`]. Returns `None` when nothing legal is left.
pub(crate) fn sanitize_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches('/').to_lowercase();
    let mut out = String::new();
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        return None;
    }
    Some(out.chars().take(NAME_MAX).collect())
}

/// The description to register: trimmed, falling back to the command name when
/// the catalog left it empty (Discord requires a non-empty CHAT_INPUT
/// description), clamped to [`DESCRIPTION_MAX`].
pub(crate) fn description_for(description: &str, name: &str) -> String {
    let trimmed = description.trim();
    let chosen = if trimmed.is_empty() { name } else { trimmed };
    chosen.chars().take(DESCRIPTION_MAX).collect()
}

/// The single option every command carries, flagged for autocomplete when its
/// values come from a live catalog (FR-005 / AC-007).
///
/// The flag is decided by [`super::autocomplete::catalog_for`], the same table
/// the interaction arm answers from, so what is registered and what is answered
/// cannot drift. A command with no catalog keeps a plain text option, which is
/// what every command had before this: Discord only sends an autocomplete
/// interaction for an option that asked for one.
pub(crate) fn command_option(source_name: &str) -> CreateCommandOption {
    let option = CreateCommandOption::new(
        CommandOptionType::String,
        ARGS_OPTION,
        ARGS_OPTION_DESCRIPTION,
    )
    .required(false);
    if super::autocomplete::catalog_for(source_name).is_some() {
        option.set_autocomplete(true)
    } else {
        option
    }
}

/// Rebuild the invocation text from a picked command's options.
///
/// The inverse of [`command_option`]: that writes the one `args` option this
/// registers, this reads it back. Both live here so the reader and the writer
/// cannot disagree about the option's name or its type.
///
/// The result is the text the user would have typed, `/name args`, which is what
/// makes a picked command and a typed message the same request. An option that
/// is not `args` is logged rather than ignored: one option is all this
/// registers, so anything else means the client sent a set we did not ask for.
pub(crate) fn invocation(command: &CommandInteraction) -> String {
    let mut text = format!("/{}", command.data.name);
    for option in &command.data.options {
        if option.name == ARGS_OPTION
            && let CommandDataOptionValue::String(value) = &option.value
            && !value.is_empty()
        {
            text.push(' ');
            text.push_str(value.as_str());
        } else if option.name != ARGS_OPTION {
            tracing::warn!(
                "Discord: unexpected option {:?} on command {:?}",
                option.name,
                command.data.name
            );
        }
    }
    text
}

/// Project the catalog onto Discord's command grammar. Catalog order is
/// preserved and the first entry wins a sanitized-name collision, which is the
/// ordering rule `trim_catalog_to_budget` already applies on Telegram, so both
/// channels drop the same entries for the same reason.
pub(crate) fn plan_commands(catalog: &[UserCommand]) -> CommandPlan {
    let mut plan = CommandPlan::default();
    let mut tree_chars = 0usize;

    for entry in catalog {
        let Some(name) = sanitize_name(&entry.name) else {
            tracing::warn!(
                "discord: dropping command {:?}: no legal Discord name after sanitizing",
                entry.name
            );
            plan.dropped.push(Dropped {
                source: entry.name.clone(),
                reason: DropReason::InvalidName,
            });
            continue;
        };

        if let Some((_, winner, _)) = plan
            .entries
            .iter()
            .find(|(registered, _, _)| registered == &name)
        {
            tracing::warn!(
                "discord: dropping command {:?}: it maps to {:?}, already used by {:?}",
                entry.name,
                name,
                winner
            );
            plan.dropped.push(Dropped {
                source: entry.name.clone(),
                reason: DropReason::Collision {
                    kept: winner.clone(),
                },
            });
            continue;
        }

        if plan.commands.len() >= GUILD_COMMAND_CAP {
            plan.dropped.push(Dropped {
                source: entry.name.clone(),
                reason: DropReason::OverCap,
            });
            continue;
        }

        let description = description_for(&entry.description, &name);
        let cost = name.chars().count()
            + description.chars().count()
            + ARGS_OPTION.len()
            + ARGS_OPTION_DESCRIPTION.len();
        if tree_chars + cost > TREE_CHAR_CAP {
            plan.dropped.push(Dropped {
                source: entry.name.clone(),
                reason: DropReason::TreeTooLarge,
            });
            continue;
        }
        tree_chars += cost;

        let builder = CreateCommand::new(name.clone())
            .description(description.clone())
            .kind(CommandType::ChatInput)
            .add_option(command_option(&entry.name));

        plan.entries.push((name, entry.name.clone(), description));
        plan.commands.push(builder);
    }

    plan
}

/// Comparison key for one sync: the command-set signature mixed with the guild
/// membership. Both matter and neither is enough alone.
///
/// The catalog part is what keeps a flapping gateway quiet: the retry loop
/// rebuilds the client every few seconds, and a reconnect that re-PUT the whole
/// tree to every guild on each pass spends the 5-per-second per-route bucket
/// for an identical result. The membership part is what keeps a guild the bot
/// joined mid-session from being missed until a config write, because joining
/// moves the key even when `commands.toml` has not been touched.
///
/// Guild ids are sorted first, so the key depends on the set and not on the
/// order the gateway happened to report them in.
pub(crate) fn sync_key(plan_sig: u64, guilds: &[GuildId]) -> u64 {
    let mut ids: Vec<u64> = guilds.iter().map(|guild| guild.get()).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut hasher = DefaultHasher::new();
    plan_sig.hash(&mut hasher);
    ids.hash(&mut hasher);
    hasher.finish()
}

/// Fold the component signatures into the one the sync key is built from.
///
/// Sequential hashing rather than `a ^ b`: XOR is its own inverse, so a
/// component that moved twice, or two components that swapped values, would
/// collide on the same key and skip a sync that was actually needed.
pub(crate) fn sync_signature(parts: &[u64]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for part in parts {
        part.hash(&mut hasher);
    }
    hasher.finish()
}

/// Built-ins offered in the Discord `/` menu beside `commands.toml` (#2013).
///
/// Each name here is answered by the interaction path (see
/// `interactions::handle_command_interaction`) and never routed to the model.
/// That pairing is the whole contract: a name in this list without a matching arm
/// there is a menu entry that throws when tapped, which is worse than an absent
/// one. Upstream landed the same idea with a single entry (`/respond_to`); this is
/// the subset of built-ins that `commands::try_execute_text_command` can already
/// answer as plain text today.
///
/// Deliberately absent, so the omission is a decision and not an oversight:
/// - `/exit` — kills the daemon from a chat surface. `/restart` is the furthest a
///   guild should reach; an exit here reproduces the silent dead-bot state this
///   channel was just debugged out of.
/// - `/new`, `/stop`, `/clear`, `/compact`, `/sessions` — session and context
///   state, driven by the TUI and the session switcher, not by text alone.
/// - `/goal`, `/profiles`, `/rename`, `/cd`, `/cowork` — persistent config, or a
///   QR handshake; `/cowork` is Telegram-only by its own description.
/// - `/discard`, `/execute` — the plan card already owns these in Discord.
pub(crate) const MENU_BUILTINS: &[&str] = &[
    "/help",
    "/usage",
    "/models",
    "/architecture",
    "/attach",
    "/audit",
    "/rtk",
    "/mission-control",
    "/plan",
    "/show-plan",
    "/respond_to",
    "/redact",
    "/restart",
    "/evolve",
];

/// Append the Discord-menu built-ins that the interaction path can answer.
///
/// Descriptions come from `commands::builtin_catalog()`, the same list `/help`
/// prints, so the menu and `/help` cannot drift apart. A name in
/// [`MENU_BUILTINS`] that is missing from that catalog is skipped with a warning
/// rather than advertised with an invented description: the catalog is the source
/// of truth, and the unit test pins the two together so the warning should never
/// fire.
///
/// Dedupe is by raw name, and the `commands.toml` entry wins. That file is the
/// source of truth on every surface, so a built-in must never shadow a command
/// the user actually wrote — including one written on purpose to override a
/// built-in.
pub(crate) fn with_menu_builtins(mut catalog: Vec<UserCommand>) -> Vec<UserCommand> {
    let known = crate::channels::commands::builtin_catalog();
    for name in MENU_BUILTINS {
        if catalog.iter().any(|c| c.name == *name) {
            continue;
        }
        let Some((_, description)) = known.iter().find(|(n, _)| n == name) else {
            tracing::warn!(
                "discord: menu built-in {} is missing from commands::builtin_catalog()",
                name
            );
            continue;
        };
        catalog.push(UserCommand {
            name: (*name).to_string(),
            description: (*description).to_string(),
            action: "system".to_string(),
            prompt: String::new(),
        });
    }
    catalog
}

/// Load `commands.toml` and register it globally, but only when the comparison
/// key moved. Returns the key that is live now, for the caller to store and hand
/// back on the next call, or `None` when nothing could be registered, which tells
/// the caller not to remember this attempt as done.
///
/// Global rather than guild-scoped because a DM has no guild, and Discord serves
/// a DM's command list only from the global set. The global write lands first:
/// if it fails the guild-scoped set is still there to fall back on, whereas
/// clearing first and then failing would leave the menu empty.
///
/// An empty catalog syncs an empty command list rather than being skipped: the
/// file is the source of truth on every other surface, so a command the user
/// deleted must not keep living in Discord's menu pointing at a prompt that no
/// longer exists. That case is logged, because wiping the menu is visible enough
/// to be worth a line.
///
/// Skipping an unchanged re-read is the point of the key: the config watcher
/// fires on every write, and a sync that re-sends an identical set costs an API
/// round-trip for nothing. The 5-per-second per-route bucket
/// (`application-commands.mdx:206`) is what a chatty watcher would hit, not the
/// 200-per-day create budget, which bulk overwrite does not touch.
pub(crate) async fn sync_commands(
    http: &Arc<Http>,
    guilds: &[GuildId],
    last_key: Option<u64>,
) -> Option<u64> {
    let catalog =
        with_menu_builtins(CommandLoader::from_brain_path(&BrainLoader::resolve_path()).load());

    if catalog.is_empty() {
        tracing::info!("discord: command catalog is empty, syncing an empty global list");
    }

    let plan = plan_commands(&catalog);
    // FR-006: the context menus travel in the same overwrite.
    // `set_global_commands` replaces the whole global set, so registering them
    // in a call of their own would erase the catalog, and the next catalog sync
    // would erase them. Both components feed the key, so an edit to either one
    // re-syncs.
    let menus = super::context_menu::commands();
    let key = sync_key(
        sync_signature(&[plan.signature(), super::context_menu::signature()]),
        guilds,
    );
    if Some(key) == last_key {
        tracing::debug!(
            "discord: application commands unchanged ({} registered), skipping sync",
            plan.commands.len() + menus.len()
        );
        return Some(key);
    }

    let mut commands = plan.commands;
    commands.extend(menus);

    match Command::set_global_commands(http, commands).await {
        Ok(registered) => {
            tracing::info!(
                "discord: synced {} application command(s) globally",
                registered.len()
            );
        }
        Err(why) => {
            // A bot invited before this feature existed has the `bot` scope but
            // not `applications.commands`, and every call here answers 403.
            // Naming the fix in the log is the only way an operator reading one
            // line knows to re-invite instead of restart. The key stays unset so
            // the next reconnect retries: the operator usually re-invites
            // without restarting and without touching `commands.toml`, and
            // remembering the failure as done would leave the menu empty after a
            // fix that looks like it did not work.
            tracing::error!(
                "discord: global application command sync failed: {why}. If this is \
                 a permissions error, the bot was invited without the \
                 `applications.commands` OAuth2 scope: re-invite it with \
                 `bot applications.commands`."
            );
            return None;
        }
    }

    // The guild-scoped set is what this feature used to write. A guild-scoped
    // command shadows a global one of the same name, so leaving the old copies
    // behind would keep a command deleted from `commands.toml` alive in that
    // guild's menu. Clear it now that the global set is live. Best effort: a
    // failure here is logged and does not undo the global registration.
    for guild in guilds {
        if let Err(why) = guild.set_commands(http, Vec::new()).await {
            tracing::warn!(
                "discord: could not clear the guild-scoped command set for \
                 {guild}: {why}. The global set is live and the menu still \
                 works, but a command removed from `commands.toml` may \
                 linger here until the next sync."
            );
        }
    }

    Some(key)
}

/// The OC-02 deny-by-default verdict, the rule `handle_message` applies to
/// typed text and this channel's only access control. An empty allowlist with
/// no roles and no owner is UNCONFIGURED, and unconfigured denies everybody:
/// the old "empty means everyone" reading made a half-configured Discord bot
/// public. Otherwise the owner, an allowlisted id, or a holder of an allowed
/// role is admitted.
///
/// Extracted pure so the branch can be pinned without a gateway.
pub(crate) fn identity_admitted(
    unconfigured: bool,
    is_owner: bool,
    in_allowlist: bool,
    role_granted: bool,
) -> bool {
    !unconfigured && (is_owner || in_allowlist || role_granted)
}

/// Whether any of a member's role ids is on the allowlist. Ids cross a JSON
/// boundary as strings, so the comparison is stringly, exactly as the message
/// path does it. A DM has no member object and no roles, and callers must pass
/// an empty list for one: treating a DM as role-granted would let a DM walk
/// past a guild-only allowlist.
pub(crate) fn holds_allowed_role(allowed_roles: &[String], role_ids: &[u64]) -> bool {
    role_ids
        .iter()
        .any(|r| allowed_roles.iter().any(|ar| ar == &r.to_string()))
}
