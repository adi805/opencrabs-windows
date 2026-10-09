//! Discord Send Tool
//!
//! Agent-callable tool for full Discord control: send, reply, react, edit, delete,
//! pin/unpin, threads, embeds, message history, channel listing, moderation,
//! native polls, announcement webhooks, and AutoMod/audit-log access. Always
//! prefer this tool over http_request: credentials are handled securely.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolHints, ToolResult};
use crate::channels::discord::DiscordState;
use crate::channels::discord::component_spec;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::sync::Arc;

/// Tool for comprehensive Discord bot control (28 actions).
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
///
/// Zero is refused rather than passed through: every caller feeds this into a
/// serenity id constructor, and those panic on zero. A Discord id of zero does
/// not exist, so the only way to reach one is a typo, and an error is the right
/// answer to a typo.
#[allow(clippy::result_large_err)]
fn get_id(input: &Value, key: &str) -> std::result::Result<u64, ToolResult> {
    match input.get(key).and_then(|v| v.as_str()) {
        Some(s) => match s.parse::<u64>() {
            Ok(0) => Err(ToolResult::error(format!(
                "Invalid {key} '0': a Discord id is never zero."
            ))),
            Ok(id) => Ok(id),
            Err(_) => Err(ToolResult::error(format!(
                "Invalid {key} '{s}': must be a numeric string."
            ))),
        },
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

/// Refuse a guild-mutating action when the current turn is a scheduled job that
/// was given no destination. See [`crate::cron::send_scope::may_moderate`].
///
/// `target` names the affected member when the action has one. An AutoMod rule
/// or an audit-log-driven change is guild-level and has no member, so those
/// callers pass `None` rather than a placeholder id.
fn moderation_guard(target: Option<u64>) -> Option<ToolResult> {
    if crate::cron::send_scope::may_moderate() {
        return None;
    }
    let reason = crate::cron::send_scope::moderation_refusal(target);
    tracing::warn!("discord_send: {reason}");
    Some(ToolResult::error(reason))
}

// ── FR-012: announcement webhooks ───────────────────────────────────────────

/// Name of the webhook an announcement leaves through. Stable on purpose: it is
/// how a later announcement recognises the first one's webhook instead of
/// minting a duplicate on every call. Discord allows 1-80 characters and
/// serenity refuses a name under 2, so this sits safely inside both.
pub(crate) const ANNOUNCE_WEBHOOK_NAME: &str = "OpenCrabs Announcements";

/// Discord's ceiling on one message. An announcement is a single crosspostable
/// message by definition, so one past the ceiling is refused rather than
/// chunked: chunking would crosspost a fragment and silently drop the rest.
pub(crate) const ANNOUNCE_MAX_CHARS: usize = 2000;

/// The parts of a webhook that decide whether it can carry our announcement.
///
/// Serenity's `Webhook` is only constructible over a live connection, so the
/// selection rule is expressed over this plain view instead: that keeps the
/// rule unit-testable without a gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebhookView {
    /// The webhook's own id.
    pub id: u64,
    /// The name it was created with.
    pub name: Option<String>,
    /// The channel it belongs to.
    pub channel_id: Option<u64>,
    /// Whether it is an incoming webhook, the only kind that can be executed.
    pub incoming: bool,
    /// Whether Discord handed us a token for it.
    pub has_token: bool,
}

/// Reduce a serenity webhook to the fields the selection rule reads.
pub(crate) fn webhook_view(webhook: &serenity::model::webhook::Webhook) -> WebhookView {
    use serenity::model::webhook::WebhookType;
    WebhookView {
        id: webhook.id.get(),
        name: webhook.name.clone(),
        channel_id: webhook.channel_id.map(|c| c.get()),
        incoming: webhook.kind == WebhookType::Incoming,
        has_token: webhook.token.is_some(),
    }
}

/// Pick the webhook a repeated announcement should reuse.
///
/// `has_token` is the load-bearing filter rather than a belt-and-braces extra:
/// Discord only returns a token for a webhook the bot may execute, so one
/// without a token cannot be used however well its name matches. Matching on
/// the name as well keeps us off a webhook some other integration owns.
pub(crate) fn pick_announce_webhook(
    views: &[WebhookView],
    channel_id: u64,
    name: &str,
) -> Option<u64> {
    views
        .iter()
        .find(|v| {
            v.incoming
                && v.has_token
                && v.channel_id == Some(channel_id)
                && v.name.as_deref() == Some(name)
        })
        .map(|v| v.id)
}

/// Resolve the reusable webhook for a channel, cloning it out of the listing.
///
/// The picked id has to come back through the serenity type, because executing
/// it needs the token that only the live object carries.
fn existing_webhook(
    listed: &[serenity::model::webhook::Webhook],
    views: &[WebhookView],
    channel_id: u64,
) -> Option<serenity::model::webhook::Webhook> {
    let id = pick_announce_webhook(views, channel_id, ANNOUNCE_WEBHOOK_NAME)?;
    listed.iter().find(|w| w.id.get() == id).cloned()
}

/// Whether a channel kind supports crossposting (PRD FR-012).
///
/// Only announcement channels do. Discord answers `400` on any other kind, so
/// the check happens before the call and its verdict is reported as a note.
pub(crate) fn crosspostable(kind: serenity::model::channel::ChannelType) -> bool {
    kind == serenity::model::channel::ChannelType::News
}

/// Refuse an announcement Discord would reject, with the measured length.
pub(crate) fn check_announce_length(text: &str) -> std::result::Result<(), String> {
    let len = text.chars().count();
    if len > ANNOUNCE_MAX_CHARS {
        return Err(format!(
            "Announcement is {len} characters; Discord allows {ANNOUNCE_MAX_CHARS} in one \
             message, and an announcement is not split because only one message can be \
             crossposted. Shorten it."
        ));
    }
    Ok(())
}

// ── FR-013: AutoMod rules and the audit log ─────────────────────────────────

/// Default name for a rule we create, so a later call recognises it.
pub(crate) const AUTOMOD_DEFAULT_NAME: &str = "OpenCrabs blocked phrase";

/// The reason recorded on every AutoMod change we make, so the guild's own
/// audit log says where the change came from rather than only that it happened.
pub(crate) const AUTOMOD_AUDIT_REASON: &str = "OpenCrabs agent: AutoMod rule change";

/// Discord's ceiling on keywords in one rule.
pub(crate) const AUTOMOD_MAX_KEYWORDS: usize = 1000;

/// Discord's ceiling on one keyword, in characters.
pub(crate) const AUTOMOD_MAX_KEYWORD_CHARS: usize = 60;

/// Shorten text for an error message without splitting a character.
pub(crate) fn truncate_for_display(text: &str) -> String {
    const MAX: usize = 40;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX).collect();
    format!("{head}...")
}

/// Read a keyword list from the parameter text.
///
/// Commas and newlines both separate, so a caller can paste a list either way.
/// Duplicates collapse: Discord counts the entries, so a repeated phrase spends
/// the budget without widening the rule.
pub(crate) fn parse_keywords(spec: &str) -> std::result::Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for part in spec.split([',', '\n']) {
        let word = part.trim();
        if word.is_empty() {
            continue;
        }
        let len = word.chars().count();
        if len > AUTOMOD_MAX_KEYWORD_CHARS {
            return Err(format!(
                "Keyword '{}' is {len} characters; Discord allows \
                 {AUTOMOD_MAX_KEYWORD_CHARS} per keyword. Split it into shorter phrases.",
                truncate_for_display(word)
            ));
        }
        if !out.iter().any(|k| k == word) {
            out.push(word.to_string());
        }
    }
    if out.is_empty() {
        return Err(
            "No keywords given: pass at least one phrase for the rule to block.".to_string(),
        );
    }
    if out.len() > AUTOMOD_MAX_KEYWORDS {
        return Err(format!(
            "{} keywords given; Discord allows {AUTOMOD_MAX_KEYWORDS} in one rule.",
            out.len()
        ));
    }
    Ok(out)
}

/// What an AutoMod rule looks like once reduced to the fields worth reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleView {
    /// The rule's own id.
    pub id: u64,
    /// Its display name.
    pub name: String,
    /// Whether Discord is currently enforcing it.
    pub enabled: bool,
    /// The event context, rendered.
    pub event: String,
    /// What fires it, rendered.
    pub trigger: String,
    /// What it does, rendered.
    pub actions: String,
}

/// One line describing what fires a rule.
///
/// The final arm is a wildcard rather than `Trigger::Unknown` because the enum
/// is `#[non_exhaustive]`: a new variant upstream has to render as "something
/// else", not fail to compile here.
pub(crate) fn trigger_label(trigger: &serenity::model::guild::automod::Trigger) -> String {
    use serenity::model::guild::automod::Trigger;
    match trigger {
        Trigger::Keyword {
            strings,
            regex_patterns,
            allow_list,
        } => {
            let mut parts = vec![format!("{} keyword(s)", strings.len())];
            if !regex_patterns.is_empty() {
                parts.push(format!("{} regex", regex_patterns.len()));
            }
            if !allow_list.is_empty() {
                parts.push(format!("{} allowed", allow_list.len()));
            }
            parts.join(", ")
        }
        Trigger::Spam => "spam".to_string(),
        Trigger::KeywordPreset {
            presets,
            allow_list,
        } => {
            let mut parts = vec![format!("{} preset(s)", presets.len())];
            if !allow_list.is_empty() {
                parts.push(format!("{} allowed", allow_list.len()));
            }
            parts.join(", ")
        }
        Trigger::MentionSpam {
            mention_total_limit,
        } => format!("more than {mention_total_limit} mention(s)"),
        other => format!("unrecognised trigger ({other:?})"),
    }
}

/// One line describing what a rule does when it fires.
pub(crate) fn automod_action_label(action: &serenity::model::guild::automod::Action) -> String {
    use serenity::model::guild::automod::Action;
    match action {
        Action::BlockMessage { custom_message } => match custom_message {
            Some(text) => format!("block (says: {})", truncate_for_display(text)),
            None => "block".to_string(),
        },
        Action::Alert(channel) => format!("alert to channel {}", channel.get()),
        Action::Timeout(duration) => format!("timeout {}s", duration.as_secs()),
        other => format!("unrecognised action ({other:?})"),
    }
}

/// Reduce a serenity AutoMod rule to the fields the report reads.
pub(crate) fn rule_view(rule: &serenity::model::guild::automod::Rule) -> RuleView {
    RuleView {
        id: rule.id.get(),
        name: rule.name.clone(),
        enabled: rule.enabled,
        event: format!("{:?}", rule.event_type),
        trigger: trigger_label(&rule.trigger),
        actions: rule
            .actions
            .iter()
            .map(automod_action_label)
            .collect::<Vec<_>>()
            .join(" + "),
    }
}

/// Render one rule as a line of the listing.
pub(crate) fn render_rule(view: &RuleView) -> String {
    let state = if view.enabled { "enabled" } else { "disabled" };
    format!(
        "- {} [{}] id={} event={} trigger={} actions={}",
        view.name, state, view.id, view.event, view.trigger, view.actions
    )
}

/// What one audit-log entry looks like once reduced to the fields worth
/// reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditEntryView {
    /// The entry's own id, which is how a moderator points at it.
    pub id: u64,
    /// What happened, rendered.
    pub action: String,
    /// Who did it.
    pub user_id: u64,
    /// What it was done to, when Discord names a target.
    pub target_id: Option<u64>,
    /// The reason the actor gave, when they gave one.
    pub reason: Option<String>,
    /// The AutoMod rule involved, which is how a blocked message is attributed.
    pub rule_name: Option<String>,
}

/// A readable label for an audit-log action.
///
/// serenity gives `Action` no `Display` impl and nests a different inner enum
/// per category, so the category is named here and the detail is carried as
/// `{:?}`: matching the inner enums would break on any upstream addition for no
/// gain, since the top level is what a moderator filters by.
pub(crate) fn audit_action_label(action: serenity::model::guild::audit_log::Action) -> String {
    use serenity::model::guild::audit_log::Action;
    match action {
        Action::AutoMod(inner) => format!("automod {inner:?}"),
        Action::Member(inner) => format!("member {inner:?}"),
        Action::Message(inner) => format!("message {inner:?}"),
        Action::Channel(inner) => format!("channel {inner:?}"),
        Action::Role(inner) => format!("role {inner:?}"),
        Action::Webhook(inner) => format!("webhook {inner:?}"),
        Action::Thread(inner) => format!("thread {inner:?}"),
        Action::GuildUpdate => "guild update".to_string(),
        other => format!("{other:?}"),
    }
}

/// Reduce a serenity audit-log entry to the fields the report reads.
pub(crate) fn audit_entry_view(
    entry: &serenity::model::guild::audit_log::AuditLogEntry,
) -> AuditEntryView {
    AuditEntryView {
        id: entry.id.get(),
        action: audit_action_label(entry.action),
        user_id: entry.user_id.get(),
        target_id: entry.target_id.map(|t| t.get()),
        reason: entry.reason.clone(),
        rule_name: entry
            .options
            .as_ref()
            .and_then(|o| o.auto_moderation_rule_name.clone()),
    }
}

/// Render one audit-log entry as a line, without mentioning anyone: the id is
/// the handle, so echoing a user id as a ping would only spam the channel.
pub(crate) fn render_audit_entry(entry: &AuditEntryView) -> String {
    let mut line = format!("- {} by user {}", entry.action, entry.user_id);
    if let Some(target) = entry.target_id {
        line.push_str(&format!(" on {target}"));
    }
    if let Some(rule) = &entry.rule_name {
        line.push_str(&format!(" (rule: {rule})"));
    }
    if let Some(reason) = &entry.reason {
        line.push_str(&format!(": {}", truncate_for_display(reason)));
    }
    line.push_str(&format!(" [entry {}]", entry.id));
    line
}

/// Map a filter name, or a raw Discord action number, to an audit-log filter.
///
/// The names cover the categories a moderator actually asks about. An unknown
/// name is refused rather than guessed at, because Discord's filter is exact and
/// a wrong one silently returns the wrong slice of the log.
pub(crate) fn parse_audit_action(spec: &str) -> Option<serenity::model::guild::audit_log::Action> {
    use serenity::model::guild::audit_log::Action;
    let code: u8 = match spec.to_ascii_lowercase().as_str() {
        "automod_rule_create" => 140,
        "automod_rule_update" => 141,
        "automod_rule_delete" => 142,
        "automod_block_message" => 143,
        "member_kick" => 20,
        "member_ban" => 22,
        "member_update" => 24,
        "member_role_update" => 25,
        "message_delete" => 72,
        "webhook_create" => 50,
        other => other.parse::<u8>().ok()?,
    };
    Some(Action::from_value(code))
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
         rename members, kick and ban members, post native polls (send_poll), post an \
         announcement through a channel webhook (announce), manage AutoMod rules \
         (automod_list/automod_create/automod_edit/automod_delete), and read the guild audit log \
         (audit_log). Always use discord_send instead of http_request: credentials handled \
         securely."
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
                        "send_file", "send_select", "send_form", "send_poll",
                        "announce",
                        "automod_list", "automod_create", "automod_edit", "automod_delete",
                        "audit_log",
                    ],
                    "description": "The Discord action to perform"
                },
                "message": {
                    "type": "string",
                    "description": "Message text (send, reply, edit, announce) or embed \
                                    description (send_embed)"
                },
                "channel_id": {
                    "type": "string",
                    "description": "Discord channel ID (numeric string). Omit to use owner's last \
                                    channel."
                },
                "message_id": {
                    "type": "string",
                    "description": "Target message ID for \
                                    reply/react/unreact/edit/delete/pin/unpin/create_thread"
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
                    "description": "Body text for send_embed (single-embed path; ignored when \
                                    'embeds' is given)"
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
                    "description": "Multi-embed layout for send_embed (max 10 blocks). When \
                                    present and non-empty it replaces \
                                    embed_title/embed_description/embed_color. Each block: title \
                                    (<=256 chars), description (<=4096), color (RGB int, default \
                                    Discord blurple). Discord caps the combined title+description \
                                    text across all blocks at 6000 chars; overflow is trimmed \
                                    from the tail and reported."
                },
                "thread_name": {
                    "type": "string",
                    "description": "Thread name for create_thread"
                },
                "user_id": {
                    "type": "string",
                    "description": "Target user ID (numeric string) for \
                                    add_role/remove_role/kick/ban/timeout/nickname, or for \
                                    audit_log the only actor whose entries are returned"
                },
                "role_id": {
                    "type": "string",
                    "description": "Role ID (numeric string) for add_role/remove_role"
                },
                "duration": {
                    "type": "string",
                    "description": "Timeout length for the timeout action: 30s, 10m, 2h or 7d, or \
                                    a count of seconds. Discord caps a timeout at 28 days; \
                                    anything longer is refused, so use the ban action for a \
                                    permanent removal."
                },
                "nickname": {
                    "type": "string",
                    "description": "New nickname for the nickname action (1-32 characters; \
                                    Discord refuses 'everyone', 'here' and 'discord')."
                },
                "reason": {
                    "type": "string",
                    "description": "Optional audit-log reason recorded for \
                                    timeout/nickname/kick/ban/add_role/remove_role. Discord shows \
                                    it in the guild audit log."
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of messages to fetch for get_messages (1-100, default \
                                    10), or of audit-log entries for audit_log (1-100, default 10)"
                },
                "keywords": {
                    "type": "string",
                    "description": "AutoMod keyword list for automod_create/automod_edit, \
                                    separated by commas or newlines (each phrase up to 60 \
                                    characters). A phrase containing spaces blocks that whole \
                                    phrase."
                },
                "rule_id": {
                    "type": "string",
                    "description": "AutoMod rule ID (numeric string) for automod_edit and \
                                    automod_delete"
                },
                "name": {
                    "type": "string",
                    "description": "Rule name for automod_create (defaults to 'OpenCrabs blocked \
                                    phrase') or a new name for automod_edit"
                },
                "enabled": {
                    "type": "boolean",
                    "description": "For automod_edit: whether Discord should enforce the rule"
                },
                "block_message": {
                    "type": "string",
                    "description": "For automod_create: the message shown to a member whose post \
                                    is blocked (Discord caps it at 150 characters)"
                },
                "alert_channel_id": {
                    "type": "string",
                    "description": "For automod_create: a channel ID (numeric string) to log \
                                    blocked content to, in addition to blocking it"
                },
                "audit_action": {
                    "type": "string",
                    "description": "For audit_log: filter by action, either a name \
                                    (automod_rule_create, automod_block_message, member_kick, \
                                    member_ban, member_update, member_role_update, \
                                    message_delete, webhook_create) or a raw Discord action \
                                    number. Omit for every action."
                },
                "options": {
                    "type": "array",
                    "maxItems": 25,
                    "items": {
                        "oneOf": [
                            {"type": "string"},
                            {
                                "type": "object",
                                "properties": {
                                    "label": {"type": "string"},
                                    "description": {"type": "string"},
                                    "emoji": {"type": "string"},
                                    "default": {"type": "boolean"}
                                },
                                "required": ["label"]
                            }
                        ]
                    },
                    "description": "Choices for send_select (max 25). A bare string is the label; \
                                    an object adds a description, a unicode emoji, and default \
                                    (pre-selected when the menu opens). The user's pick is routed \
                                    back to you as a new turn."
                },
                "placeholder": {
                    "type": "string",
                    "description": "Placeholder text for send_select's menu."
                },
                "min_values": {
                    "type": "integer",
                    "description": "For send_select: fewest options the user may pick (1-25). \
                                    Defaults to 1."
                },
                "max_values": {
                    "type": "integer",
                    "description": "For send_select: most options the user may pick (1-25, capped \
                                    at the option count). multi_select is sugar for this."
                },
                "title": {
                    "type": "string",
                    "description": "Modal title for send_form."
                },
                "fields": {
                    "type": "array",
                    "maxItems": 5,
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": {"type": "string"},
                            "multiline": {"type": "boolean"},
                            "placeholder": {"type": "string"},
                            "required": {"type": "boolean"},
                            "min_length": {"type": "integer"},
                            "max_length": {"type": "integer"},
                            "value": {"type": "string"}
                        },
                        "required": ["label"]
                    },
                    "description": "Form fields for send_form (max 5). label and multiline are \
                                    the base shape; placeholder, required (default true), \
                                    min_length, max_length and value are optional refinements. \
                                    Submitted values are routed back to you as a new turn."
                },
                "poll_question": {
                    "type": "string",
                    "description": "Poll question text for send_poll. Discord caps it at 300 \
                                    chars; longer is truncated"
                },
                "poll_options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Poll answer strings for send_poll. Discord allows up to 10 \
                                    answers; blank entries are dropped and labels over 55 chars \
                                    are truncated"
                },
                "poll_duration_hours": {
                    "type": "integer",
                    "description": "How long a send_poll stays open, in hours (1-768, the \
                                    platform 32-day ceiling). Defaults to 24"
                },
                "multi_select": {
                    "type": "boolean",
                    "description": "For send_poll: let voters pick several answers. For \
                                    send_select: sugar for max_values = the option count, so the \
                                    user may pick any number. Default false (single choice)"
                },
                "file_path": {
                    "type": "string",
                    "description": "Local file path to upload (required for send_file). Refused \
                                    locally if over Discord's 20 MiB per-attachment default or 25 \
                                    MiB request limit."
                },
                "caption": {
                    "type": "string",
                    "description": "Optional caption text for send_file"
                },
                "silent": {
                    "type": "boolean",
                    "description": "Post with SUPPRESS_NOTIFICATIONS: recipients get the unread \
                                    badge but no push/desktop notification. Applies to send, \
                                    reply, send_embed, send_file. Omit to use the channel default \
                                    (channels.discord.suppress_notifications, false). Set true \
                                    for scheduled/report output that should not ping the server; \
                                    set false to force a loud send on a quiet channel."
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

        use serenity::model::id::{ChannelId, GuildId, MessageId, RoleId, RuleId, UserId};

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
                if let Some(refused) = moderation_guard(Some(user_id)) {
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
                if let Some(refused) = moderation_guard(Some(user_id)) {
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
                if let Some(refused) = moderation_guard(Some(user_id)) {
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
                if let Some(refused) = moderation_guard(Some(user_id)) {
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
                if let Some(refused) = moderation_guard(Some(user_id)) {
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
                if let Some(refused) = moderation_guard(Some(user_id)) {
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
                use serenity::model::channel::ReactionType;
                let text = pget!(get_str(&input, "message")).to_string();
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let raw_options = input
                    .get("options")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let options = component_spec::parse_select_options(&raw_options);
                if options.is_empty() {
                    return Ok(ToolResult::error(
                        "send_select needs a non-empty 'options' array.".to_string(),
                    ));
                }
                let multi_select = input
                    .get("multi_select")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let (min_values, max_values) = component_spec::select_arity(
                    multi_select,
                    options.len(),
                    input
                        .get("min_values")
                        .and_then(|v| v.as_u64())
                        .and_then(|n| u8::try_from(n).ok()),
                    input
                        .get("max_values")
                        .and_then(|v| v.as_u64())
                        .and_then(|n| u8::try_from(n).ok()),
                );
                let select_id = uuid::Uuid::new_v4().to_string();
                let menu_options: Vec<CreateSelectMenuOption> = options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| {
                        let mut opt = CreateSelectMenuOption::new(o.label.clone(), i.to_string());
                        if let Some(description) = &o.description {
                            opt = opt.description(description.clone());
                        }
                        if let Some(emoji) = &o.emoji {
                            opt = opt.emoji(ReactionType::Unicode(emoji.clone()));
                        }
                        if o.default {
                            opt = opt.default_selection(true);
                        }
                        opt
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
                if let Some(min) = min_values {
                    menu = menu.min_values(min);
                }
                if let Some(max) = max_values {
                    menu = menu.max_values(max);
                }
                let message = CreateMessage::new()
                    .content(text)
                    .components(vec![CreateActionRow::SelectMenu(menu)]);
                match ChannelId::new(channel_id)
                    .send_message(&http, message)
                    .await
                {
                    Ok(_) => {
                        let labels = options.iter().map(|o| o.label.clone()).collect();
                        self.discord_state.register_select(select_id, labels).await;
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
                let raw_fields = input
                    .get("fields")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let fields = component_spec::parse_form_fields(&raw_fields);
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

            // ── announce (FR-012 / AC-015) ───────────────────────────────────
            // Post through a webhook carrying the bot's own name and avatar,
            // then crosspost so the message is marked published.
            "announce" => {
                use crate::channels::discord::writes;
                use serenity::builder::{Builder, CreateWebhook, ExecuteWebhook};
                let channel_id = pget!(channel_or_err(channel_id_opt));
                let channel = ChannelId::new(channel_id);
                let raw = pget!(get_str(&input, "message")).to_string();
                let text = crate::channels::discord::table_convert::tables_to_discord(&raw);
                if let Err(e) = check_announce_length(&text) {
                    return Ok(ToolResult::error(e));
                }

                // Reuse this channel's announcement webhook when there is one,
                // so a repeated announcement does not litter the channel with a
                // new webhook per call.
                let listed = match http.get_channel_webhooks(channel).await {
                    Ok(list) => list,
                    Err(e) => {
                        return Ok(ToolResult::error(format!(
                            "Failed to list webhooks for channel {channel_id}: {e}"
                        )));
                    }
                };
                let views: Vec<WebhookView> = listed.iter().map(webhook_view).collect();
                let reuse = existing_webhook(&listed, &views, channel_id);
                let webhook = match reuse {
                    Some(w) => w,
                    None => {
                        let builder = CreateWebhook::new(ANNOUNCE_WEBHOOK_NAME);
                        match builder.execute(&http, channel).await {
                            Ok(w) => w,
                            Err(e) => {
                                return Ok(ToolResult::error(format!(
                                    "Failed to create an announcement webhook in channel \
                                     {channel_id} (needs Manage Webhooks): {e}"
                                )));
                            }
                        }
                    }
                };

                // The webhook posts as the bot itself: `username`/`avatar_url`
                // are what carry the bot's identity, which is the half of
                // AC-015 that is about the webhook rather than the channel.
                let bot = match http.get_current_user().await {
                    Ok(u) => (u.name.clone(), u.face()),
                    Err(e) => {
                        return Ok(ToolResult::error(format!(
                            "Failed to read the bot identity for the webhook override: {e}"
                        )));
                    }
                };
                let builder = ExecuteWebhook::new()
                    .content(text.as_str())
                    .username(bot.0.as_str())
                    .avatar_url(bot.1.as_str());
                let outcome = writes::execute_webhook(&http, channel, &webhook, builder).await;
                let posted = match outcome {
                    Ok(Some(m)) => m,
                    Ok(None) => {
                        return Ok(ToolResult::error(
                            "The announcement was refused by the Discord write budget; \
                             nothing was posted."
                                .to_string(),
                        ));
                    }
                    Err(e) => {
                        return Ok(ToolResult::error(format!("Announcement post failed: {e}")));
                    }
                };

                // Crossposting is what marks the message published, and only an
                // announcement channel supports it. The post has already
                // landed, so a refusal here is reported as a note: it never
                // turns a delivered announcement into a failed one.
                let is_news = match http.get_channel(channel).await {
                    Ok(serenity::model::channel::Channel::Guild(gc)) => crosspostable(gc.kind),
                    _ => false,
                };
                let crosspost = if !is_news {
                    format!("not crossposted: {channel_id} is not an announcement channel")
                } else {
                    match http.crosspost_message(channel, posted.id).await {
                        Ok(_) => "crossposted".to_string(),
                        Err(e) => format!("not crossposted: {e}"),
                    }
                };
                Ok(ToolResult::success(format!(
                    "Announcement posted to {channel_id} through webhook {} (message {}); \
                     {crosspost}.",
                    webhook.id.get(),
                    posted.id.get()
                )))
            }

            // ── automod_list (FR-013) ────────────────────────────────────────
            "automod_list" => {
                let gid = pget!(guild_or_err(guild_id_opt));
                match http.get_automod_rules(GuildId::new(gid)).await {
                    Ok(rules) if rules.is_empty() => Ok(ToolResult::success(format!(
                        "No AutoMod rules in guild {gid}."
                    ))),
                    Ok(rules) => {
                        let mut out = format!("{} AutoMod rule(s) in guild {gid}:\n", rules.len());
                        for rule in &rules {
                            out.push_str(&render_rule(&rule_view(rule)));
                            out.push('\n');
                        }
                        Ok(ToolResult::success(out))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to list AutoMod rules (needs Manage Guild): {e}"
                    ))),
                }
            }

            // ── automod_create (FR-013) ──────────────────────────────────────
            "automod_create" => {
                use serenity::builder::{Builder, EditAutoModRule};
                use serenity::model::guild::automod::{Action as AutomodAction, Trigger};
                let gid = pget!(guild_or_err(guild_id_opt));
                if let Some(refusal) = moderation_guard(None) {
                    return Ok(refusal);
                }
                let raw = pget!(get_str(&input, "keywords")).to_string();
                let keywords = match parse_keywords(&raw) {
                    Ok(k) => k,
                    Err(e) => return Ok(ToolResult::error(e)),
                };
                let name = input
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .unwrap_or(AUTOMOD_DEFAULT_NAME);
                let mut actions = vec![AutomodAction::BlockMessage {
                    custom_message: input
                        .get("block_message")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                }];
                if let Some(spec) = input.get("alert_channel_id").and_then(|v| v.as_str()) {
                    match spec.parse::<u64>() {
                        Ok(0) | Err(_) => {
                            return Ok(ToolResult::error(format!(
                                "Invalid alert_channel_id '{spec}': must be a non-zero numeric \
                                 string"
                            )));
                        }
                        Ok(id) => actions.push(AutomodAction::Alert(ChannelId::new(id))),
                    }
                }
                let trigger = Trigger::Keyword {
                    strings: keywords.clone(),
                    regex_patterns: Vec::new(),
                    allow_list: Vec::new(),
                };
                let builder = EditAutoModRule::new()
                    .name(name)
                    .trigger(trigger)
                    .actions(actions)
                    .enabled(true)
                    .audit_log_reason(AUTOMOD_AUDIT_REASON);
                let ctx = (GuildId::new(gid), None);
                match builder.execute(&http, ctx).await {
                    Ok(rule) => Ok(ToolResult::success(format!(
                        "Created AutoMod rule '{}' (id {}) in guild {gid}, blocking {} \
                         keyword(s): {}",
                        rule.name,
                        rule.id.get(),
                        keywords.len(),
                        keywords.join(", ")
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to create the AutoMod rule (needs Manage Guild): {e}"
                    ))),
                }
            }

            // ── automod_edit (FR-013) ────────────────────────────────────────
            "automod_edit" => {
                use serenity::builder::{Builder, EditAutoModRule};
                use serenity::model::guild::automod::Trigger;
                let gid = pget!(guild_or_err(guild_id_opt));
                if let Some(refusal) = moderation_guard(None) {
                    return Ok(refusal);
                }
                let rule_id = pget!(get_id(&input, "rule_id"));
                let mut builder = EditAutoModRule::new().audit_log_reason(AUTOMOD_AUDIT_REASON);
                let mut changed: Vec<&str> = Vec::new();
                let new_name = input
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|n| !n.is_empty());
                if let Some(name) = new_name {
                    builder = builder.name(name);
                    changed.push("name");
                }
                if let Some(enabled) = input.get("enabled").and_then(|v| v.as_bool()) {
                    builder = builder.enabled(enabled);
                    changed.push("enabled");
                }
                if let Some(raw) = input.get("keywords").and_then(|v| v.as_str()) {
                    let keywords = match parse_keywords(raw) {
                        Ok(k) => k,
                        Err(e) => return Ok(ToolResult::error(e)),
                    };
                    let trigger = Trigger::Keyword {
                        strings: keywords,
                        regex_patterns: Vec::new(),
                        allow_list: Vec::new(),
                    };
                    builder = builder.trigger(trigger);
                    changed.push("keywords");
                }
                if changed.is_empty() {
                    return Ok(ToolResult::error(
                        "Nothing to change: pass at least one of 'name', 'enabled' or \
                         'keywords'."
                            .to_string(),
                    ));
                }
                let ctx = (GuildId::new(gid), Some(RuleId::new(rule_id)));
                match builder.execute(&http, ctx).await {
                    Ok(rule) => Ok(ToolResult::success(format!(
                        "Updated AutoMod rule '{}' (id {}): changed {}.",
                        rule.name,
                        rule.id.get(),
                        changed.join(", ")
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to update AutoMod rule {rule_id} (needs Manage Guild): {e}"
                    ))),
                }
            }

            // ── automod_delete (FR-013) ──────────────────────────────────────
            "automod_delete" => {
                let gid = pget!(guild_or_err(guild_id_opt));
                if let Some(refusal) = moderation_guard(None) {
                    return Ok(refusal);
                }
                let rule_id = pget!(get_id(&input, "rule_id"));
                let guild = GuildId::new(gid);
                let rule = RuleId::new(rule_id);
                let reason = Some(AUTOMOD_AUDIT_REASON);
                match http.delete_automod_rule(guild, rule, reason).await {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Deleted AutoMod rule {rule_id} from guild {gid}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to delete AutoMod rule {rule_id} (needs Manage Guild): {e}"
                    ))),
                }
            }

            // ── audit_log (FR-013 / AC-016) ──────────────────────────────────
            "audit_log" => {
                let gid = pget!(guild_or_err(guild_id_opt));
                let limit = input
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10)
                    .clamp(1, 100) as u8;
                let filter = input.get("audit_action").and_then(|v| v.as_str());
                let action = match filter {
                    Some(spec) if !spec.trim().is_empty() => {
                        match parse_audit_action(spec.trim()) {
                            Some(a) => Some(a),
                            None => {
                                return Ok(ToolResult::error(format!(
                                    "Unknown audit_action '{}'. Use a name \
                                     (automod_rule_create, automod_block_message, member_kick, \
                                     member_ban, member_update, member_role_update, \
                                     message_delete, webhook_create) or a raw Discord action \
                                     number.",
                                    truncate_for_display(spec)
                                )));
                            }
                        }
                    }
                    _ => None,
                };
                let user_id = match input.get("user_id").and_then(|v| v.as_str()) {
                    Some(spec) => match spec.parse::<u64>() {
                        Ok(0) | Err(_) => {
                            return Ok(ToolResult::error(format!(
                                "Invalid user_id '{spec}': must be a non-zero numeric string"
                            )));
                        }
                        Ok(id) => Some(UserId::new(id)),
                    },
                    None => None,
                };
                let guild = GuildId::new(gid);
                let entries = http
                    .get_audit_logs(guild, action, user_id, None, Some(limit))
                    .await;
                match entries {
                    Ok(log) if log.entries.is_empty() => Ok(ToolResult::success(format!(
                        "No audit-log entries in guild {gid} match that filter."
                    ))),
                    Ok(log) => {
                        let mut out = format!(
                            "{} audit-log entr(ies) in guild {gid}:\n",
                            log.entries.len()
                        );
                        for entry in &log.entries {
                            out.push_str(&render_audit_entry(&audit_entry_view(entry)));
                            out.push('\n');
                        }
                        Ok(ToolResult::success(out))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to read the audit log (needs View Audit Log): {e}"
                    ))),
                }
            }

            unknown => Ok(ToolResult::error(format!(
                "Unknown action '{unknown}'. Valid: send, reply, react, unreact, edit, delete, \
                 send_select, send_form, \
                 send_poll, pin, unpin, create_thread, send_embed, get_messages, \
                 list_channels, add_role, remove_role, kick, ban, timeout, nickname, send_file, \
                 announce, automod_list, automod_create, automod_edit, automod_delete, audit_log"
            ))),
        }
    }
}
