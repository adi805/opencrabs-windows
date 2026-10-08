//! Right-click entry points (FR-006 / AC-009).
//!
//! Two context-menu commands: `Ask agent` on a message and `Ask agent about
//! user` on a member. They are the same entry point as a slash command, so they
//! ride the same interaction arm and get the same deny-by-default gate, the
//! same deferred ack and the same turn router. The only difference is where the
//! request text comes from: a chat-input command has arguments, a context menu
//! has a target, and [`invocation`] turns that target into the text.
//!
//! Both are registered in the SAME global overwrite as the catalog, which is
//! the trap this module exists to keep shut. `Command::set_global_commands`
//! replaces the whole global set, so registering the context menus in a second
//! call would erase the catalog, and the next catalog sync would erase them.
//!
//! Neither is a `CHAT_INPUT` command, and Discord's rules differ for the other
//! two types (`developers/interactions/application-commands.mdx`): mixed case
//! and spaces are legal in the name (`:49`), the description must be empty
//! (`:62`), and the budgets are separate (15 `USER`, 15 `MESSAGE`, `:199`), so
//! these two cost the catalog nothing.
//!
//! A context menu is unavailable in a DM (Discord serves none there), so there
//! is no DM branch to write.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serenity::builder::CreateCommand;
use serenity::model::application::{CommandInteraction, CommandType};

/// Right-click a message.
pub(crate) const MESSAGE_COMMAND: &str = "Ask agent";

/// Right-click a member.
pub(crate) const USER_COMMAND: &str = "Ask agent about user";

/// Discord's cap on a command name, in characters.
///
/// The same 32 as [`super::commands::NAME_MAX`] but a different rule: this one
/// constrains the length only. The `^[\w-]{1,32}$` charset applies to
/// `CHAT_INPUT`, and `USER`/`MESSAGE` names are explicitly allowed mixed case
/// and spaces, so [`MESSAGE_COMMAND`] and [`USER_COMMAND`] are legal as
/// written and must not be run through `sanitize_name`.
///
/// `cfg(test)` because only the test reads it: `clippy --lib` compiles without
/// `cfg(test)` and `-D warnings` turns an unread constant into a failed build.
#[cfg(test)]
pub(crate) const NAME_MAX: usize = 32;

/// The registered set, in one table.
///
/// Two readers derive from it and nothing else declares these names:
/// [`commands`] builds the builders and [`signature`] hashes what they put on
/// the wire, so the registration and the sync key cannot drift. Same argument
/// as `autocomplete::catalog_for`.
pub(crate) const ENTRIES: [(&str, CommandType); 2] = [
    (MESSAGE_COMMAND, CommandType::Message),
    (USER_COMMAND, CommandType::User),
];

/// The builders, ready to travel with the catalog in the global overwrite.
///
/// No `.description(...)` and no options. Serenity leaves `description` out of
/// the payload when it is unset, which is what Discord's own example sends for
/// a `USER` command (`application-commands.mdx:286`), and a description is
/// required of `CHAT_INPUT` alone.
pub(crate) fn commands() -> Vec<CreateCommand> {
    ENTRIES
        .iter()
        .map(|(name, kind)| CreateCommand::new(*name).kind(*kind))
        .collect()
}

/// A signature over the registered set, for the sync key.
///
/// Hashed from this table rather than from the builders: a builder does not
/// expose its fields, so the alternative would be a signature that cannot see
/// what it is signing.
pub(crate) fn signature() -> u64 {
    let mut hasher = DefaultHasher::new();
    for (name, kind) in ENTRIES {
        name.hash(&mut hasher);
        kind_code(kind).hash(&mut hasher);
    }
    hasher.finish()
}

/// Discord's numeric command type, as it goes on the wire.
///
/// Spelled out rather than cast: `CommandType` is `#[non_exhaustive]`, so the
/// catch-all arm is mandatory, and a wire number is the thing the signature has
/// to move on when it changes.
fn kind_code(kind: CommandType) -> u8 {
    match kind {
        CommandType::User => 2,
        CommandType::Message => 3,
        _ => 0,
    }
}

/// The request text for a context-menu invocation.
///
/// `None` means the client named a target it did not resolve. Discord does that
/// when the message was deleted between the right-click and the interaction
/// landing; there is then nothing to ask about, and the caller answers a
/// refusal instead of running a turn on a message nobody can see.
pub(crate) fn invocation(command: &CommandInteraction) -> Option<String> {
    let target = command.data.target_id?;
    match command.data.kind {
        CommandType::Message => {
            let message = command
                .data
                .resolved
                .messages
                .get(&target.to_message_id())?;
            Some(message_prompt(
                &message.author.name,
                message.author.id.get(),
                &message.link(),
                &message.content,
            ))
        }
        CommandType::User => {
            let user = command.data.resolved.users.get(&target.to_user_id())?;
            Some(user_prompt(&user.name, user.id.get()))
        }
        _ => None,
    }
}

/// The request text for a right-clicked message.
///
/// The content is passed through verbatim rather than summarised: the agent is
/// being asked about THIS message, and a paraphrase would answer about a
/// message nobody sent. The author id travels with the name because a display
/// name is not stable and two members can share one, and the link so the answer
/// can point back at what it is talking about.
///
/// An attachment-only message has no content, and the placeholder says so
/// instead of leaving the request looking truncated: the agent cannot open a
/// Discord link, so the honest signal is that there was no text, not a silent
/// empty block it might fill in from context.
pub(crate) fn message_prompt(author: &str, author_id: u64, link: &str, content: &str) -> String {
    let body = content.trim();
    let body = if body.is_empty() {
        "(no text content)"
    } else {
        body
    };
    format!("Ask about this message.\nAuthor: {author} ({author_id})\nLink: {link}\n\n{body}")
}

/// The request text for a right-clicked member.
///
/// The id is the payload, not the name: it is what every other surface in this
/// channel addresses a user by, and the name is only there to make the request
/// readable in the transcript.
pub(crate) fn user_prompt(name: &str, id: u64) -> String {
    format!("Ask about this user.\nName: {name}\nId: {id}")
}
