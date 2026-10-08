//! Discord Send Tool
//!
//! Agent-callable tool for full Discord control: send, reply, react, edit, delete,
//! pin/unpin, threads, embeds, message history, channel listing, moderation, and
//! native polls. Always prefer this tool over http_request: credentials are
//! handled securely.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolHints, ToolResult};
use crate::channels::discord::DiscordState;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::sync::Arc;

/// Tool for comprehensive Discord bot control (22 actions).
pub struct DiscordSendTool {
    discord_state: Arc<DiscordState>,
}

impl DiscordSendTool {
    pub fn new(discord_state: Arc<DiscordState>) -> Self {
        Self { discord_state }
    }
}

/// Extract a required non-empty string param, returning ToolResult::error on failure.
#[allow(clippy::result_large_err)]
fn get_str<'a>(input: &'a Value, key: &str) -> std::result::Result<&'a str, ToolResult> {
    match input.get(key).and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(ToolResult::error(format!(
            "Missing required parameter '{key}'."
        ))),
    }
}

/// Parse a required numeric-string param as u64.
#[allow(clippy::result_large_err)]
fn get_id(input: &Value, key: &str) -> std::result::Result<u64, ToolResult> {
    match input.get(key).and_then(|v| v.as_str()) {
        Some(s) => s.parse::<u64>().map_err(|_| {
            ToolResult::error(format!("Invalid {key} '{s}': must be a numeric string."))
        }),
        None => Err(ToolResult::error(format!(
            "Missing required parameter '{key}'."
        ))),
    }
}

/// Unwrap channel id or return error ToolResult.
#[allow(clippy::result_large_err)]
fn channel_or_err(id: Option<u64>) -> std::result::Result<u64, ToolResult> {
    let raw = id.ok_or_else(|| {
        ToolResult::error(
            "No channel_id provided and no owner channel available. \
             The owner must send a message first, or pass channel_id explicitly."
                .to_string(),
        )
    })?;
    if !crate::cron::send_scope::may_send("discord", &raw.to_string()) {
        let reason = crate::cron::send_scope::refusal_for("discord", &raw.to_string());
        tracing::warn!("discord_send: {reason}");
        return Err(ToolResult::error(reason));
    }
    Ok(raw)
}

/// Unwrap guild id or return error ToolResult.
#[allow(clippy::result_large_err)]
fn guild_or_err(id: Option<u64>) -> std::result::Result<u64, ToolResult> {
    id.ok_or_else(|| {
        ToolResult::error(
            "No guild ID available. The bot must receive at least one guild message first."
                .to_string(),
        )
    })
}

/// Discord's ceiling on a communication timeout: 28 days. Anything past it is
/// rejected by the API, which is why [`parse_timeout_secs`] refuses instead of
/// clamping.
const TIMEOUT_MAX_SECS: i64 = 28 * 24 * 60 * 60;

/// Discord's ceiling on a nickname, in characters.
const NICKNAME_MAX_CHARS: usize = 32;

/// Read a timeout length into seconds (FR-009).
///
/// A caller writes either a compact duration (`30s`, `10m`, `2h`, `7d`) or a
/// bare count of seconds, so both are accepted. A length that is empty,
/// unreadable, zero, or past the 28-day ceiling is refused with the reason
/// rather than clamped: silently shortening a request hides the mismatch from
/// whoever asked for it, and a timeout is an action taken against a person.
pub(crate) fn parse_timeout_secs(spec: &str) -> std::result::Result<i64, String> {
    let text = spec.trim();
    if text.is_empty() {
        return Err("Missing required parameter 'duration'.".to_string());
    }
    // The unit is the final character, which is ASCII when it matches, so the
    // byte slice lands on a char boundary.
    let (digits, unit_secs) = match text.chars().last() {
        Some('s') | Some('S') => (&text[..text.len() - 1], 1),
        Some('m') | Some('M') => (&text[..text.len() - 1], 60),
        Some('h') | Some('H') => (&text[..text.len() - 1], 3600),
        Some('d') | Some('D') => (&text[..text.len() - 1], 86_400),
        _ => (text, 1),
    };
    let value: i64 = match digits.trim().parse() {
        Ok(value) => value,
        Err(_) => {
            return Err(format!(
                "Cannot read duration '{spec}': use 30s, 10m, 2h, 7d or a count of seconds."
            ));
        }
    };
    if value <= 0 {
        return Err(format!(
            "A timeout of '{spec}' is not a timeout: give a length above zero (30s, 10m, 2h, 7d)."
        ));
    }
    match value.checked_mul(unit_secs) {
        Some(secs) if secs <= TIMEOUT_MAX_SECS => Ok(secs),
        _ => Err(format!(
            "'{spec}' is longer than the 28 days Discord allows for a timeout; use the ban \
             action for a permanent removal."
        )),
    }
}

/// The instant a timeout of `secs` seconds ends, as the ISO8601 string
/// `EditMember::disable_communication_until` wants.
pub(crate) fn timeout_until_rfc3339(secs: i64, now: DateTime<Utc>) -> String {
    let until = now + chrono::Duration::seconds(secs);
    until.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Check a requested nickname against Discord's own rules (FR-009): 1 to 32
/// characters, and none of the three reserved words it refuses outright. The
/// trimmed form is what gets sent, so a name that is only spaces is refused
/// rather than posted.
pub(crate) fn validate_nickname(name: &str) -> std::result::Result<&str, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Missing required parameter 'nickname'.".to_string());
    }
    let count = trimmed.chars().count();
    if count > NICKNAME_MAX_CHARS {
        return Err(format!(
            "Nickname is {count} characters; Discord allows at most {NICKNAME_MAX_CHARS}."
        ));
    }
    let lowered = trimmed.to_ascii_lowercase();
    if matches!(lowered.as_str(), "everyone" | "here" | "discord") {
        return Err(format!(
            "Discord refuses the reserved nickname '{trimmed}'; pick another."
        ));
    }
    Ok(trimmed)
}

/// Refuse a member-mutating action when the current turn is a scheduled job
/// that was given no destination. See [`crate::cron::send_scope::may_moderate`].
fn moderation_guard(user_id: u64) -> Option<ToolResult> {
    if crate::cron::send_scope::may_moderate() {
        return None;
    }
    let reason = crate::cron::send_scope::moderation_refusal(user_id);
    tracing::warn!("discord_send: {reason}");
    Some(ToolResult::error(reason))
}

// Macro to early-return Ok(err_result) when a param helper returns Err.
macro_rules! pget {
    ($expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => return Ok(e),
        }
    };
}

#[async_trait]
impl Tool for DiscordSendTool {
    fn name(&self) -> &str {
        "discord_send"
    }

    fn description(&self) -> &str {
        "Full Discord control: send messages, reply, react, edit, delete, pin/unpin, create \
         threads, send embeds, fetch message history, list channels, manage roles, time out and \
         rename members, kick and ban members, and post native polls (send_poll). Always use \
         discord_send instead of http_request: credentials handled securely."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "send", "reply", "react", "unreact", "edit", "delete",
                        "pin", "unpin", "create_thread", "send_embed", "get_messages",
                        "list_channels", "add_role", "remove_role", "kick", "ban",
                        "timeout", "nickname",
                        "send_file", "send_select", "send_form", "send_poll"
                    ],
                    "description": "The Discord action to perform"
                },
                "message": {
                    "type": "string",
                    "description": "Message text (send, reply, edit) or embed description (send_embed)"
                },
                "channel_id": {
                    "type": "string",
                    "description": "Discord channel ID (numeric string). Omit to use owner's last channel."
                },
                "message_id": {
                    "type": "string",
                    "description": "Target message ID for reply/react/unreact/edit/delete/pin/unpin/create_thread"
                },
                "emoji": {
                    "type": "string",
                    "description": "Unicode emoji for react/unreact (e.g. \"👍\")"
                },
                "embed_title": {
                    "type": "string",
                    "description": "Title for send_embed"
                },
                "embed_description": {
                    "type": "string",
                    "description": "Body text for send_embed (single-embed path; ignored when 'embeds' is given)"
                },
                "embed_color": {
                    "type": "integer",
                    "description": "RGB color integer for send_embed (e.g. 0x00FF00 = 65280)"
                },
                "embeds": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "description": {"type": "string"},
                            "color": {"type": "integer"}
                        }
                    },
                    "description": "Multi-embed layout for send_embed (max 10 blocks). When present and non-empty it replaces embed_title/embed_description/embed_color. Each block: title (<=256 chars), description (<=4096), color (RGB int, default Discord blurple). Discord caps the combined title+description text across all blocks at 6000 chars; overflow is trimmed from the tail and reported."
                },
                "thread_name": {
                    "type": "string",
                    "description": "Thread name for create_thread"
                },
                "user_id": {
                    "type": "string",
                    "description": "Target user ID (numeric string) for add_role/remove_role/kick/ban/timeout/nickname"
                },
                "role_id": {
                    "type": "string",
                    "description": "Role ID (numeric string) for add_role/remove_role"
                },
                "duration": {
                    "type": "string",
                    "description": "Timeout length for the timeout action: 30s, 10m, 2h or 7d, or a count of seconds. Discord caps a timeout at 28 days; anything longer is refused, so use the ban action for a permanent removal."
                },
                "nickname": {
                    "type": "string",
                    "description": "New nickname for the nickname action (1-32 characters; Discord refuses 'everyone', 'here' and 'discord')."
                },
                "reason": {
                    "type": "string",
                    "description": "Optional audit-log reason recorded for timeout/nickname/kick/ban/add_role/remove_role. Discord shows it in the guild audit log."
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of messages to fetch for get_messages (1-100, default 10)"
                },
                "options": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Choices for send_select (max 25). The user's pick is routed back to you as a new turn."
                },
                "placeholder": {
                    "type": "string",
                    "description": "Placeholder text for send_select's menu."
                },
                "title": {
                    "type": "string",
                    "description": "Modal title for send_form."
                },
                "fields": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": {"type": "string"},
                            "multiline": {"type": "boolean"}
                        },
                        "required": ["label"]
                    },
                    "description": "Form fields for send_form (max 5). Submitted values are routed back to you as a new turn."
                },
                "poll_question": {
                    "type": "string",
                    "description": "Poll question text for send_poll. Discord caps it at 300 chars; longer is truncated"
                },
                "poll_options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Poll answer strings for send_poll. Discord allows up to 10 answers; blank entries are dropped and labels over 55 chars are truncated"
                },
                "poll_duration_hours": {
                    "type": "integer",
                    "description": "How long a send_poll stays open, in hours (1-768, the platform 32-day ceiling). Defaults to 24"
                },
                "multi_select": {
                    "type": "boolean",
                    "description": "For send_poll: let voters pick several answers. Default false (single choice)"
                },
                "file_path": {
                    "type": "string",
                    "description": "Local file path to upload (required for send_file). Refused locally if over Discord's 20 MiB per-attachment default or 25 MiB request limit."
                },
                "caption": {
                    "type": "string",
                    "description": "Optional caption text for send_file"
                },
                "silent": {
                    "type": "boolean",
                    "description": "Post with SUPPRESS_NOTIFICATIONS: recipients get the unread badge but no push/desktop notification. Applies to send, reply, send_embed, send_file. Omit to use the channel default (channels.discord.suppress_notifications, false). Set true for scheduled/report output that should not ping the server; set false to force a loud send on a quiet channel."
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    fn hints(&self) -> ToolHints {
        ToolHints {
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        }
    }

    async fn execute(&self, input: Value, _context: &ToolExecutionContext) -> Result<ToolResult> {
        let action = match input.get("action").and_then(|v| v.as_str()) {
            Some(a) if !a.is_empty() => a.to_string(),
            _ => {
                return Ok(ToolResult::error(
                    "Missing required 'action' parameter.".to_string(),
                ));
            }
        };

        let http = match self.discord_state.http().await {
            Some(h) => h,
            None => {
                return Ok(ToolResult::error(
                    "Discord is not connected. Run discord_connect first.".to_string(),
                ));
            }
        };

        // Resolve target channel: explicit param > ambient origin fallback > owner's last channel
        let channel_id_opt = if let Some(id_str) = input.get("channel_id").and_then(|v| v.as_str())
        {
            match id_str.parse::<u64>() {
                Ok(id) => Some(id),
                Err(_) => {
                    return Ok(ToolResult::error(format!(
                        "Invalid channel_id '{id_str}': must be a numeric string"
                    )));
                }
            }
        } else if let Some(origin) = _context.origin_target.as_deref() {
            if origin.channel == "discord" {
                origin.chat_id.parse::<u64>().ok()
            } else {
                self.discord_state.owner_channel_id().await
            }
        } else {
            self.discord_state.owner_channel_id().await
        };

        let guild_id_opt = self.discord_state.guild_id().await;

        // C1: resolve silent delivery once. An explicit `silent` param wins
        // over the channel default so one scheduled job can stay loud on a
        // quiet channel and vice versa. Only the actions that create a fresh
        // message consult it (send/reply/send_embed/send_file); edit/react and
        // the rest never set create-time flags.
        let silent = crate::channels::discord::flags::resolve_silent(
            input.get("silent").and_then(|v| v.as_bool()),
            crate::config::Config::current()
                .channels
                .discord
                .suppress_notifications,
        );

        use serenity::model::id::{ChannelId, GuildId, MessageId, RoleId, UserId};

        match action.as_str() {
            // ── send ─────────────────────────────────────────────────────────
            "send" => {
                use serenity::builder::CreateMessage;
                let text = pget!(get_str(&input, "message")).to_string();
                let text = crate::channels::discord::table_convert::tables_to_discord(&text);
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let channel = ChannelId::new(channel_id);
                let chunks = crate::channels::discord::handler::split_message(&text, 2000);
                for chunk in chunks {
                    // C1: build through apply_silent rather than `channel.say`,
                    // which has no flags field on its builder.
                    let builder = crate::channels::discord::flags::apply_silent(
                        CreateMessage::new().content(chunk),
                        silent,
                    );
                    if let Err(e) = channel.send_message(&http, builder).await {
                        return Ok(ToolResult::error(format!("Failed to send: {e}")));
                    }
                }
                Ok(ToolResult::success(format!(
                    "Message sent to channel {channel_id}."
                )))
            }

            // ── reply ────────────────────────────────────────────────────────
            "reply" => {
                use serenity::builder::CreateMessage;
                use serenity::model::channel::MessageReference;
                let text = pget!(get_str(&input, "message")).to_string();
                let text = crate::channels::discord::table_convert::tables_to_discord(&text);
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let message_id = pget!(get_id(&input, "message_id"));
                let channel = ChannelId::new(channel_id);
                let reference = MessageReference::from((channel, MessageId::new(message_id)));
                let builder = crate::channels::discord::flags::apply_silent(
                    CreateMessage::new()
                        .content(text.as_str())
                        .reference_message(reference),
                    silent,
                );
                match channel.send_message(&http, builder).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Reply sent to message {message_id}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to reply: {e}"))),
                }
            }

            // ── react ────────────────────────────────────────────────────────
            "react" => {
                use serenity::model::channel::ReactionType;
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let message_id = pget!(get_id(&input, "message_id"));
                let emoji = pget!(get_str(&input, "emoji")).to_string();
                let reaction = ReactionType::Unicode(emoji.clone());
                match http
                    .create_reaction(
                        ChannelId::new(channel_id),
                        MessageId::new(message_id),
                        &reaction,
                    )
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Reacted with {emoji} on message {message_id}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to react: {e}"))),
                }
            }

            // ── unreact ──────────────────────────────────────────────────────
            "unreact" => {
                use serenity::model::channel::ReactionType;
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let message_id = pget!(get_id(&input, "message_id"));
                let emoji = pget!(get_str(&input, "emoji")).to_string();
                let reaction = ReactionType::Unicode(emoji.clone());
                match http
                    .delete_reaction_me(
                        ChannelId::new(channel_id),
                        MessageId::new(message_id),
                        &reaction,
                    )
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Removed reaction {emoji} from message {message_id}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to remove reaction: {e}"))),
                }
            }

            // ── edit ─────────────────────────────────────────────────────────
            "edit" => {
                use serenity::builder::EditMessage;
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let message_id = pget!(get_id(&input, "message_id"));
                let text = pget!(get_str(&input, "message")).to_string();
                let text = crate::channels::discord::table_convert::tables_to_discord(&text);
                let edit = EditMessage::new().content(text.as_str());
                match http
                    .edit_message(
                        ChannelId::new(channel_id),
                        MessageId::new(message_id),
                        &edit,
                        vec![],
                    )
                    .await
                {
                    Ok(_) => Ok(ToolResult::success(format!("Message {message_id} edited."))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to edit message: {e}"))),
                }
            }

            // ── delete ───────────────────────────────────────────────────────
            "delete" => {
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let message_id = pget!(get_id(&input, "message_id"));
                match http
                    .delete_message(ChannelId::new(channel_id), MessageId::new(message_id), None)
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Message {message_id} deleted."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to delete message: {e}"))),
                }
            }

            // ── pin ──────────────────────────────────────────────────────────
            "pin" => {
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let message_id = pget!(get_id(&input, "message_id"));
                match http
                    .pin_message(ChannelId::new(channel_id), MessageId::new(message_id), None)
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!("Message {message_id} pinned."))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to pin message: {e}"))),
                }
            }

            // ── unpin ────────────────────────────────────────────────────────
            "unpin" => {
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let message_id = pget!(get_id(&input, "message_id"));
                match http
                    .unpin_message(ChannelId::new(channel_id), MessageId::new(message_id), None)
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Message {message_id} unpinned."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to unpin message: {e}"))),
                }
            }

            // ── create_thread ────────────────────────────────────────────────
            "create_thread" => {
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let message_id = pget!(get_id(&input, "message_id"));
                let thread_name = pget!(get_str(&input, "thread_name")).to_string();
                let body = serde_json::json!({ "name": thread_name });
                match http
                    .create_thread_from_message(
                        ChannelId::new(channel_id),
                        MessageId::new(message_id),
                        &body,
                        None,
                    )
                    .await
                {
                    Ok(ch) => Ok(ToolResult::success(format!(
                        "Thread '{}' created (id={}).",
                        ch.name, ch.id
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to create thread: {e}"))),
                }
            }

            // ── send_embed ───────────────────────────────────────────────────
            "send_embed" => {
                use crate::channels::discord::embed::{EmbedInput, build_spec, embed_builders};
                use serenity::builder::CreateMessage;
                let channel_id = pget!(channel_or_err(channel_id_opt));

                // An explicit non-empty `embeds` array wins; otherwise the
                // single-embed params become a one-element input, so both paths
                // share one validator (C2). A blank single embed is refused
                // rather than sent as an empty embed Discord would 400 on.
                let inputs: Vec<EmbedInput> = match input.get("embeds").and_then(|v| v.as_array()) {
                    Some(arr) if !arr.is_empty() => arr
                        .iter()
                        .map(|e| {
                            EmbedInput::new(
                                e.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                                e.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                                e.get("color").and_then(|v| v.as_u64()).unwrap_or(0x5865F2) as u32,
                            )
                        })
                        .collect(),
                    _ => vec![EmbedInput::new(
                        input
                            .get("embed_title")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        input
                            .get("embed_description")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        input
                            .get("embed_color")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0x5865F2) as u32, // Discord blurple default
                    )],
                };

                let spec = match build_spec(&inputs) {
                    Ok(spec) => spec,
                    Err(e) => return Ok(ToolResult::error(e.message())),
                };
                let builder = crate::channels::discord::flags::apply_silent(
                    CreateMessage::new().embeds(embed_builders(&spec)),
                    silent,
                );
                match ChannelId::new(channel_id)
                    .send_message(&http, builder)
                    .await
                {
                    Ok(_) => {
                        let mut note = format!(
                            "{} embed(s) sent to channel {channel_id}.",
                            spec.embeds.len()
                        );
                        if spec.dropped_embeds > 0 {
                            note.push_str(&format!(
                                " {} block(s) dropped (blank, past the 10-embed cap, or past the \
                                 6000-char budget).",
                                spec.dropped_embeds
                            ));
                        }
                        if spec.truncated {
                            note.push_str(
                                " Text was trimmed to fit Discord's 6000-character embed budget.",
                            );
                        }
                        Ok(ToolResult::success(note))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to send embed: {e}"))),
                }
            }

            // ── get_messages ─────────────────────────────────────────────────
            "get_messages" => {
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let limit = input
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.min(100) as u8)
                    .unwrap_or(10);
                match http
                    .get_messages(ChannelId::new(channel_id), None, Some(limit))
                    .await
                {
                    Ok(messages) => {
                        let summary = messages
                            .iter()
                            .map(|m| {
                                format!(
                                    "[{}] {}: {}",
                                    m.id,
                                    m.author.name,
                                    &m.content[..m.content.floor_char_boundary(80)]
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        Ok(ToolResult::success(format!(
                            "Last {} messages in channel {channel_id}:\n{summary}",
                            messages.len()
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to fetch messages: {e}"))),
                }
            }

            // ── list_channels ────────────────────────────────────────────────
            "list_channels" => {
                let gid = pget!(guild_or_err(guild_id_opt));
                match http.get_channels(GuildId::new(gid)).await {
                    Ok(channels) => {
                        let list = channels
                            .iter()
                            .map(|c| format!("{}: {} ({})", c.id, c.name, c.kind.name()))
                            .collect::<Vec<_>>()
                            .join("\n");
                        Ok(ToolResult::success(format!(
                            "Channels in guild {gid}:\n{list}"
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to list channels: {e}"))),
                }
            }

            // ── add_role ─────────────────────────────────────────────────────
            "add_role" => {
                let gid = pget!(guild_or_err(guild_id_opt));
                let user_id = pget!(get_id(&input, "user_id"));
                let role_id = pget!(get_id(&input, "role_id"));
                if let Some(refused) = moderation_guard(user_id) {
                    return Ok(refused);
                }
                let reason = input.get("reason").and_then(|v| v.as_str());
                match http
                    .add_member_role(
                        GuildId::new(gid),
                        UserId::new(user_id),
                        RoleId::new(role_id),
                        reason,
                    )
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Role {role_id} added to user {user_id}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to add role: {e}"))),
                }
            }

            // ── remove_role ──────────────────────────────────────────────────
            "remove_role" => {
                let gid = pget!(guild_or_err(guild_id_opt));
                let user_id = pget!(get_id(&input, "user_id"));
                let role_id = pget!(get_id(&input, "role_id"));
                if let Some(refused) = moderation_guard(user_id) {
                    return Ok(refused);
                }
                let reason = input.get("reason").and_then(|v| v.as_str());
                match http
                    .remove_member_role(
                        GuildId::new(gid),
                        UserId::new(user_id),
                        RoleId::new(role_id),
                        reason,
                    )
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Role {role_id} removed from user {user_id}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to remove role: {e}"))),
                }
            }

            // ── kick ─────────────────────────────────────────────────────────
            "kick" => {
                let gid = pget!(guild_or_err(guild_id_opt));
                let user_id = pget!(get_id(&input, "user_id"));
                if let Some(refused) = moderation_guard(user_id) {
                    return Ok(refused);
                }
                let reason = input.get("reason").and_then(|v| v.as_str());
                match http
                    .kick_member(GuildId::new(gid), UserId::new(user_id), reason)
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!("User {user_id} kicked."))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to kick user: {e}"))),
                }
            }

            // ── ban ──────────────────────────────────────────────────────────
            "ban" => {
                let gid = pget!(guild_or_err(guild_id_opt));
                let user_id = pget!(get_id(&input, "user_id"));
                if let Some(refused) = moderation_guard(user_id) {
                    return Ok(refused);
                }
                let reason = input.get("reason").and_then(|v| v.as_str());
                match http
                    .ban_user(GuildId::new(gid), UserId::new(user_id), 0, reason)
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!("User {user_id} banned."))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to ban user: {e}"))),
                }
            }

            // ── timeout (FR-009 / AC-012) ────────────────────────────────────
            "timeout" => {
                use serenity::builder::EditMember;
                let gid = pget!(guild_or_err(guild_id_opt));
                let user_id = pget!(get_id(&input, "user_id"));
                if let Some(refused) = moderation_guard(user_id) {
                    return Ok(refused);
                }
                let duration = pget!(get_str(&input, "duration")).to_string();
                let secs = match parse_timeout_secs(&duration) {
                    Ok(secs) => secs,
                    Err(why) => return Ok(ToolResult::error(why)),
                };
                let until = timeout_until_rfc3339(secs, Utc::now());
                let reason = input.get("reason").and_then(|v| v.as_str());
                let mut edit = EditMember::new().disable_communication_until(until.clone());
                if let Some(reason) = reason {
                    edit = edit.audit_log_reason(reason);
                }
                match http
                    .edit_member(GuildId::new(gid), UserId::new(user_id), &edit, reason)
                    .await
                {
                    Ok(member) => {
                        let confirmed = member
                            .communication_disabled_until
                            .map(|t| t.to_string())
                            .unwrap_or_else(|| "none".to_string());
                        Ok(ToolResult::success(format!(
                            "User {user_id} timed out until {until} (requested {duration}). \
                             Discord reports the member's timeout as {confirmed}."
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to time out user: {e}"))),
                }
            }

            // ── nickname (FR-009 / AC-012) ───────────────────────────────────
            "nickname" => {
                use serenity::builder::EditMember;
                let gid = pget!(guild_or_err(guild_id_opt));
                let user_id = pget!(get_id(&input, "user_id"));
                if let Some(refused) = moderation_guard(user_id) {
                    return Ok(refused);
                }
                let requested = pget!(get_str(&input, "nickname")).to_string();
                let name = match validate_nickname(&requested) {
                    Ok(name) => name.to_string(),
                    Err(why) => return Ok(ToolResult::error(why)),
                };
                let reason = input.get("reason").and_then(|v| v.as_str());
                let mut edit = EditMember::new().nickname(name.clone());
                if let Some(reason) = reason {
                    edit = edit.audit_log_reason(reason);
                }
                let message = format!("User {user_id} renamed to '{name}'.");
                match http
                    .edit_member(GuildId::new(gid), UserId::new(user_id), &edit, reason)
                    .await
                {
                    Ok(_) => Ok(ToolResult::success(message)),
                    Err(e) => Ok(ToolResult::error(format!("Failed to rename user: {e}"))),
                }
            }

            // ── send_select (#382) ──────────────────────────────────────────
            "send_select" => {
                use serenity::builder::{
                    CreateActionRow, CreateMessage, CreateSelectMenu, CreateSelectMenuKind,
                    CreateSelectMenuOption,
                };
                let text = pget!(get_str(&input, "message")).to_string();
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let options: Vec<String> = input
                    .get("options")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .take(25)
                            .collect()
                    })
                    .unwrap_or_default();
                if options.is_empty() {
                    return Ok(ToolResult::error(
                        "send_select requires a non-empty 'options' array.".to_string(),
                    ));
                }
                let select_id = uuid::Uuid::new_v4().to_string();
                let menu_options: Vec<CreateSelectMenuOption> = options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| {
                        let label: String = o.chars().take(100).collect();
                        CreateSelectMenuOption::new(label, i.to_string())
                    })
                    .collect();
                let mut menu = CreateSelectMenu::new(
                    format!("sel:{select_id}"),
                    CreateSelectMenuKind::String {
                        options: menu_options,
                    },
                );
                if let Some(ph) = input.get("placeholder").and_then(|v| v.as_str()) {
                    menu = menu.placeholder(ph);
                }
                let message = CreateMessage::new()
                    .content(text)
                    .components(vec![CreateActionRow::SelectMenu(menu)]);
                match ChannelId::new(channel_id)
                    .send_message(&http, message)
                    .await
                {
                    Ok(_) => {
                        self.discord_state.register_select(select_id, options).await;
                        Ok(ToolResult::success(
                            "Select menu posted; the pick will arrive as a new turn.".to_string(),
                        ))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to send select: {e}"))),
                }
            }

            // send_poll (#1848: parity with Telegram and WhatsApp)
            "send_poll" => {
                use crate::channels::discord::poll::{answer_builders, build_spec};
                use serenity::builder::{CreateMessage, CreatePoll};
                use std::time::Duration;
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let question = pget!(get_str(&input, "poll_question")).to_string();
                let options: Vec<String> = input
                    .get("poll_options")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let duration = input.get("poll_duration_hours").and_then(|v| v.as_i64());
                let multi_select = input
                    .get("multi_select")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                // Validation and clamping live in poll.rs so the platform limits
                // carry tests; the builder chain below is typestate-guarded.
                let spec = match build_spec(&question, &options, duration, multi_select) {
                    Ok(spec) => spec,
                    Err(e) => return Ok(ToolResult::error(e.message())),
                };
                let mut poll = CreatePoll::new()
                    .question(spec.question.clone())
                    .answers(answer_builders(&spec))
                    .duration(Duration::from_secs(u64::from(spec.duration_hours) * 3600));
                if spec.allow_multiselect {
                    poll = poll.allow_multiselect();
                }
                match ChannelId::new(channel_id)
                    .send_message(&http, CreateMessage::new().poll(poll))
                    .await
                {
                    Ok(_) => {
                        let mut note = format!(
                            "Poll posted to channel {channel_id}: {} answers, {}h, {}.",
                            spec.answers.len(),
                            spec.duration_hours,
                            if spec.allow_multiselect {
                                "multi-select"
                            } else {
                                "single-select"
                            }
                        );
                        if spec.dropped_answers > 0 {
                            note.push_str(&format!(
                                " {} option(s) past Discord's 10-answer limit were dropped.",
                                spec.dropped_answers
                            ));
                        }
                        if spec.duration_clamped {
                            note.push_str(" Requested duration was clamped to the platform limit.");
                        }
                        Ok(ToolResult::success(note))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to send poll: {e}"))),
                }
            }

            // ── send_form (#383) ─────────────────────────────────────────────
            "send_form" => {
                use serenity::builder::{CreateActionRow, CreateButton, CreateMessage};
                use serenity::model::application::ButtonStyle;
                let text = pget!(get_str(&input, "message")).to_string();
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let title = input
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Form")
                    .to_string();
                let fields: Vec<(String, bool)> = input
                    .get("fields")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|f| {
                                let label = f.get("label")?.as_str()?.to_string();
                                let multiline = f
                                    .get("multiline")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                Some((label, multiline))
                            })
                            .take(5)
                            .collect()
                    })
                    .unwrap_or_default();
                if fields.is_empty() {
                    return Ok(ToolResult::error(
                        "send_form requires a non-empty 'fields' array (max 5).".to_string(),
                    ));
                }
                let form_id = uuid::Uuid::new_v4().to_string();
                let message =
                    CreateMessage::new()
                        .content(text)
                        .components(vec![CreateActionRow::Buttons(vec![
                            CreateButton::new(format!("form:{form_id}"))
                                .label("📝 Open form")
                                .style(ButtonStyle::Primary),
                        ])]);
                match ChannelId::new(channel_id)
                    .send_message(&http, message)
                    .await
                {
                    Ok(_) => {
                        self.discord_state
                            .register_form(
                                form_id,
                                crate::channels::discord::interactions::FormSpec { title, fields },
                            )
                            .await;
                        Ok(ToolResult::success(
                            "Form posted; submissions will arrive as a new turn.".to_string(),
                        ))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to send form: {e}"))),
                }
            }

            "send_file" => {
                use crate::channels::discord::guard::{FileSize, check_batch};
                use serenity::builder::{CreateAttachment, CreateMessage};
                use serenity::model::id::ChannelId;
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let file_path = match input.get("file_path").and_then(|v| v.as_str()) {
                    Some(p) => p.to_string(),
                    None => {
                        return Ok(ToolResult::error(
                            "send_file requires 'file_path'.".to_string(),
                        ));
                    }
                };
                let caption = input
                    .get("caption")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let channel = ChannelId::new(channel_id);
                // C3: check the size from metadata BEFORE reading, so an
                // oversized file is refused instead of pulled into memory.
                // Discord would answer 400 after the whole upload.
                let fname = std::path::Path::new(&file_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file.png")
                    .to_string();
                match tokio::fs::metadata(&file_path).await {
                    Ok(meta) => {
                        if let Err(e) = check_batch(&[FileSize::new(fname.clone(), meta.len())]) {
                            return Ok(ToolResult::error(e.message()));
                        }
                    }
                    Err(e) => {
                        return Ok(ToolResult::error(format!(
                            "Failed to read file '{file_path}': {e}"
                        )));
                    }
                }
                match tokio::fs::read(&file_path).await {
                    Ok(bytes) => {
                        let attachment = CreateAttachment::bytes(bytes.as_slice(), fname);
                        let mut msg = CreateMessage::new().add_file(attachment);
                        if !caption.is_empty() {
                            msg = msg.content(caption);
                        }
                        let msg = crate::channels::discord::flags::apply_silent(msg, silent);
                        match channel.send_message(&http, msg).await {
                            Ok(_) => Ok(ToolResult::success("File sent.".to_string())),
                            Err(e) => Ok(ToolResult::error(format!("Failed to send file: {e}"))),
                        }
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to read file '{}': {e}",
                        file_path
                    ))),
                }
            }

            unknown => Ok(ToolResult::error(format!(
                "Unknown action '{unknown}'. Valid: send, reply, react, unreact, edit, delete, send_select, send_form, \
                 send_poll, pin, unpin, create_thread, send_embed, get_messages, \
                 list_channels, add_role, remove_role, kick, ban, timeout, nickname, send_file"
            ))),
        }
    }
}
