//! WhatsApp Send Tool
//!
//! Agent-callable tool for full WhatsApp control: send, reply, delete,
//! send photos/documents/audio/video/stickers, locations, contacts,
//! reactions, polls, typing indicators, and read receipts.
//! Uses the shared `WhatsAppState` to access the connected client.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolHints, ToolResult};
use crate::channels::whatsapp::WhatsAppState;
use crate::channels::whatsapp::broadcast;
use crate::channels::whatsapp::interactive;
use crate::channels::whatsapp::introspection::{RetryBucket, WAIT_TIMEOUT};
use crate::channels::whatsapp::newsletter::post_body;
use crate::channels::whatsapp::poll::resolve_options;
use crate::channels::whatsapp::rate_limit::GateOutcome;
use crate::config::Config;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use wacore_binary::jid::{Jid, Server};
use whatsapp_rust::features::{
    CommunitySubgroup, CreateCommunityOptions, EventCreationParams, EventResponseType,
    GroupCreateOptions, GroupDescription, GroupMetadata, GroupParticipantOptions, GroupSubject,
    JoinGroupResult, MemberAddMode, MembershipApprovalMode, NewsletterMessage, NewsletterMetadata,
    ParticipantChangeResponse, PictureType, PollVoteCiphertext, PreviousDescription, group_type,
};

/// Reported by `wa_introspect` when a runtime call needs a live session and
/// there is none to act on.
const NOT_CONNECTED: &str = "Not connected to WhatsApp, so there is no live session to act on.";

/// Tool for comprehensive WhatsApp control (14 actions).
pub struct WhatsAppSendTool {
    whatsapp_state: Arc<WhatsAppState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
}

impl WhatsAppSendTool {
    pub fn new(
        whatsapp_state: Arc<WhatsAppState>,
        config_rx: tokio::sync::watch::Receiver<Config>,
    ) -> Self {
        Self {
            whatsapp_state,
            config_rx,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a required non-empty string param, returning ToolResult::error on failure.
#[allow(clippy::result_large_err)]
pub(crate) fn get_str<'a>(input: &'a Value, key: &str) -> std::result::Result<&'a str, ToolResult> {
    match input.get(key).and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(ToolResult::error(format!(
            "Missing required parameter '{key}'."
        ))),
    }
}

/// Extract an optional f64 param.
pub(crate) fn get_f64(input: &Value, key: &str) -> Option<f64> {
    input.get(key).and_then(|v| v.as_f64())
}

/// Extract a required boolean param.
///
/// The group policy setters take the value they are setting, so an absent one
/// is an error rather than a default: silently flipping a group setting the
/// caller never named is a change with no undo on the wire.
#[allow(clippy::result_large_err)]
pub(crate) fn get_bool(input: &Value, key: &str) -> std::result::Result<bool, ToolResult> {
    match input.get(key).and_then(|v| v.as_bool()) {
        Some(b) => Ok(b),
        None => Err(ToolResult::error(format!(
            "Missing required boolean parameter '{key}'."
        ))),
    }
}

/// Macro to early-return Ok(err_result) when a param helper returns Err.
macro_rules! pget {
    ($expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => return Ok(e),
        }
    };
}

/// Pace one non-text send through the shared #1407 budget.
///
/// #1407 landed covering `send` and `reply` only, so every media send, poll
/// and reaction went straight to the wire and the budget under-counted by
/// however many of those a turn made. They cannot use `gate`: that parks an
/// over-cap send in a queue holding `{ jid, text }`, and the drainer replays
/// it as plain text, which would deliver a photo's caption as a bare message
/// and drop the image. So these pace on the bucket and, under a saturated
/// daily cap, refuse and tell the agent rather than pretending to have sent.
///
/// Gates the actions that DELIVER A MESSAGE. Receipts, presence and account
/// operations (typing, mark_read, delete, pin, block, profile) are not
/// messages and are not charged to a message budget.
macro_rules! gate_send {
    ($self:expr, $jid_str:expr, $what:expr) => {{
        let (cfg, owner) = {
            let wa = &$self.config_rx.borrow().channels.whatsapp;
            (
                wa.rate_limit.clone(),
                wa.is_owner($jid_str.split('@').next().unwrap_or(&$jid_str)),
            )
        };
        if !$self
            .whatsapp_state
            .rate_limiter
            .gate_action(&cfg, owner)
            .await
        {
            return Ok(ToolResult::error(format!(
                "Daily WhatsApp send cap reached; {} was NOT sent. Unlike text, this is \
                 not queued for retry - try again once the 24h window slides.",
                $what
            )));
        }
    }};
}

/// Resolve the WhatsApp client, returning an error if not connected.
#[allow(clippy::result_large_err)]
async fn get_client(
    whatsapp_state: &WhatsAppState,
) -> std::result::Result<Arc<whatsapp_rust::client::Client>, ToolResult> {
    whatsapp_state.client().await.ok_or_else(|| {
        ToolResult::error(
            "WhatsApp is not connected. Ask the user to connect WhatsApp first \
                 (use the whatsapp_connect tool)."
                .to_string(),
        )
    })
}

/// Resolve target JID from `phone` param, originating session, or owner fallback,
/// with allowlist check.
#[allow(clippy::result_large_err)]
pub(crate) async fn resolve_jid(
    input: &Value,
    whatsapp_state: &WhatsAppState,
    config_rx: &tokio::sync::watch::Receiver<Config>,
    session_id: uuid::Uuid,
) -> std::result::Result<(Jid, String), ToolResult> {
    if let Some(raw_target) = input.get("phone").and_then(|v| v.as_str()) {
        let allowed = &config_rx.borrow().channels.whatsapp.allowed_phones;
        let target_clean = raw_target.trim().trim_start_matches('+');
        let user_part = target_clean
            .split('@')
            .next()
            .unwrap_or(target_clean)
            .split(':')
            .next()
            .unwrap_or(target_clean);

        let phone_allowed = allowed.is_empty()
            || allowed
                .iter()
                .any(|p| p.trim_start_matches('+') == user_part);
        if !phone_allowed {
            return Err(ToolResult::error(format!(
                "Sending to {} is not permitted. This number is not in the \
                 allowed_users config.",
                raw_target
            )));
        }
        let jid_str = if target_clean.contains('@') {
            target_clean.to_string()
        } else {
            format!("{}@s.whatsapp.net", user_part)
        };
        let jid: Jid = jid_str
            .parse()
            .map_err(|e| ToolResult::error(format!("Invalid phone number format: {}", e)))?;
        if !crate::cron::send_scope::may_send("whatsapp", &jid_str) {
            let reason = crate::cron::send_scope::refusal_for("whatsapp", &jid_str);
            tracing::warn!("whatsapp_send: {reason}");
            return Err(ToolResult::error(reason));
        }
        Ok((jid, jid_str))
    } else if let Some(session_jid_str) = whatsapp_state.session_jid(session_id).await {
        let jid: Jid = session_jid_str
            .parse()
            .map_err(|e| ToolResult::error(format!("Invalid session chat JID: {}", e)))?;
        if !crate::cron::send_scope::may_send("whatsapp", &session_jid_str) {
            let reason = crate::cron::send_scope::refusal_for("whatsapp", &session_jid_str);
            tracing::warn!("whatsapp_send: {reason}");
            return Err(ToolResult::error(reason));
        }
        Ok((jid, session_jid_str))
    } else {
        let jid_str = whatsapp_state.owner_jid().await.ok_or_else(|| {
            ToolResult::error(
                "No owner phone number configured and no 'phone' parameter provided. \
                 Specify a phone number to send to."
                    .to_string(),
            )
        })?;
        let jid: Jid = jid_str
            .parse()
            .map_err(|e| ToolResult::error(format!("Invalid owner JID: {}", e)))?;
        if !crate::cron::send_scope::may_send("whatsapp", &jid_str) {
            let reason = crate::cron::send_scope::refusal_for("whatsapp", &jid_str);
            tracing::warn!("whatsapp_send: {reason}");
            return Err(ToolResult::error(reason));
        }
        Ok((jid, jid_str))
    }
}

/// Resolve the target group JID from the `group_jid` parameter.
///
/// Group JIDs are `<id>@g.us`. A bare id (no `@`) is accepted and wrapped with
/// `Jid::group`, because the invite and batch surfaces hand group ids out in
/// that form. Unlike `resolve_jid` there is no owner fallback here: a group
/// operation that names no group has no sensible default, and falling back
/// would point an admin action (kick, promote) at a DM JID.
#[allow(clippy::result_large_err)]
pub(crate) fn resolve_group_jid(input: &Value) -> std::result::Result<Jid, ToolResult> {
    resolve_group_jid_keyed(input, "group_jid")
}

/// Resolve a group JID stored under `key`.
///
/// One implementation, so a caller that names its target with a different
/// parameter (`subgroup_jid`, for a group inside a community) gets the same
/// bare-id wrapping and the same server check instead of a second copy that
/// drifts away from this one.
#[allow(clippy::result_large_err)]
fn resolve_group_jid_keyed(input: &Value, key: &str) -> std::result::Result<Jid, ToolResult> {
    let raw = get_str(input, key)?.trim();
    let jid: Jid = if raw.contains('@') {
        raw.parse()
            .map_err(|e| ToolResult::error(format!("Invalid group JID '{raw}': {e}")))?
    } else {
        Jid::group(raw)
    };
    if jid.server != Server::Group {
        return Err(ToolResult::error(format!(
            "'{raw}' is not a group JID (expected an @g.us address or a bare group id)."
        )));
    }
    let jid_str = jid.to_string();
    if !crate::cron::send_scope::may_send("whatsapp", &jid_str) {
        let reason = crate::cron::send_scope::refusal_for("whatsapp", &jid_str);
        tracing::warn!("whatsapp_send: {reason}");
        return Err(ToolResult::error(reason));
    }
    Ok(jid)
}

/// Resolve the target newsletter (channel) JID from `newsletter_jid`.
///
/// Channel JIDs are `<id>@newsletter`. A bare id is accepted and wrapped, the
/// same way `resolve_group_jid` accepts a bare group id, because the invite and
/// metadata surfaces hand channel ids out in that form. There is no owner
/// fallback: a channel action that names no channel has no sensible default,
/// and falling back would point a public post at a DM.
#[allow(clippy::result_large_err)]
pub(crate) fn resolve_newsletter_jid(input: &Value) -> std::result::Result<Jid, ToolResult> {
    let raw = get_str(input, "newsletter_jid")?.trim();
    let jid: Jid = if raw.contains('@') {
        raw.parse()
            .map_err(|e| ToolResult::error(format!("Invalid newsletter JID '{raw}': {e}")))?
    } else {
        format!("{raw}@newsletter")
            .parse()
            .map_err(|e| ToolResult::error(format!("Invalid channel id '{raw}': {e}")))?
    };
    if jid.server != Server::Newsletter {
        return Err(ToolResult::error(format!(
            "'{raw}' is not a newsletter JID (expected an @newsletter address or a bare channel id)."
        )));
    }
    let jid_str = jid.to_string();
    if !crate::cron::send_scope::may_send("whatsapp", &jid_str) {
        let reason = crate::cron::send_scope::refusal_for("whatsapp", &jid_str);
        tracing::warn!("whatsapp_send: {reason}");
        return Err(ToolResult::error(reason));
    }
    Ok(jid)
}

/// Render a channel's metadata as the block every newsletter arm returns.
///
/// One formatter for `get_metadata`, `get_metadata_by_invite`, `join`, `create`
/// and `update`: the same object reported five ways would drift into five
/// shapes, and the caller cannot tell which fields an action chose to omit.
fn describe_newsletter(m: &NewsletterMetadata) -> String {
    let mut out = format!(
        "{} ({})\n  subscribers: {}\n  state: {:?}\n  verification: {:?}",
        m.name, m.jid, m.subscriber_count, m.state, m.verification
    );
    if let Some(role) = &m.role {
        out.push_str(&format!("\n  your role: {role:?}"));
    }
    if let Some(desc) = m.description.as_deref().filter(|d| !d.is_empty()) {
        out.push_str(&format!("\n  description: {desc}"));
    }
    if let Some(code) = m.invite_code.as_deref().filter(|c| !c.is_empty()) {
        out.push_str(&format!("\n  invite code: {code}"));
    }
    out
}

/// Render a page of channel history, oldest first, one line per post.
///
/// The body comes from `post_body`, the same extractor the digest poller uses,
/// so a captioned photo reads "📷 caption" here exactly as it does in the
/// digest instead of vanishing because this arm only understood plain text.
fn describe_newsletter_messages(jid: &Jid, list: &[NewsletterMessage]) -> String {
    let mut out = format!("{} post(s) from {jid} (oldest first):", list.len());
    for m in list {
        let when = chrono::DateTime::from_timestamp(m.timestamp as i64, 0)
            .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_else(|| format!("ts={}", m.timestamp));
        let body = m
            .message
            .as_ref()
            .map(post_body)
            .unwrap_or_else(|| "[post without text]".to_string());
        let flat = body.replace('\n', " ");
        let flat: String = flat.chars().take(200).collect();
        out.push_str(&format!(
            "\n- server_id {} [{}] {:?}{} :: {}",
            m.server_id,
            when,
            m.message_type,
            if m.is_sender { " (you)" } else { "" },
            flat
        ));
    }
    out
}

/// Resolve a linked subgroup's JID from `subgroup_jid`.
///
/// A subgroup is an ordinary group, so it goes through the same wrapping and
/// the same server check as any other group JID. The name differs only because
/// a community call names two groups at once, and "the community" and "the
/// group inside it" must not be spelled the same.
#[allow(clippy::result_large_err)]
pub(crate) fn resolve_subgroup_jid(input: &Value) -> std::result::Result<Jid, ToolResult> {
    resolve_group_jid_keyed(input, "subgroup_jid")
}

/// Render a community's subgroups, one line each.
///
/// Two of the entries are not distinguishable by name: the announcement group
/// and the general chat. The server marks them, so the tag is reported rather
/// than left for the caller to guess from the subject.
fn describe_community_subgroups(jid: &Jid, list: &[CommunitySubgroup]) -> String {
    let mut out = format!("{} subgroup(s) of {jid}:", list.len());
    for sg in list {
        let mut tags: Vec<&str> = Vec::new();
        if sg.is_default_sub_group {
            tags.push("announcement");
        }
        if sg.is_general_chat {
            tags.push("general chat");
        }
        let tag = if tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tags.join(", "))
        };
        let count = sg
            .participant_count
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".to_string());
        out.push_str(&format!(
            "\n  {} '{}' ({count} participant(s)){tag}",
            sg.id, sg.subject
        ));
    }
    out
}

/// Render a group's metadata as the community surface reports it.
///
/// `group_type` reads the flags the server sets, so a caller asking about a
/// subgroup learns whether it is the community's announcement group, its
/// general chat, an ordinary linked group, or a community in its own right,
/// which no single boolean in the metadata says on its own.
fn describe_community_group(m: &GroupMetadata) -> String {
    let admins = m.participants.iter().filter(|p| p.is_admin()).count();
    let mut out = format!(
        "{} ('{}') [{:?}]\n  participants: {}\n  admins: {}\n  locked: {}\n  \
         announcement-only: {}",
        m.id,
        m.subject,
        group_type(m),
        m.size.unwrap_or(m.participants.len() as u32),
        admins,
        m.is_locked,
        m.is_announcement,
    );
    if let Some(desc) = m.description.as_deref().filter(|d| !d.is_empty()) {
        out.push_str(&format!("\n  description: {desc}"));
    }
    out
}

/// Name the groups the server refused, with the code it gave.
///
/// A link request reports per-group outcomes, so a partial success that named
/// only the count would read as a clean sweep and leave the caller believing
/// the missing groups are linked.
fn describe_failed_groups(failed: &[(Jid, u32)]) -> String {
    if failed.is_empty() {
        return String::new();
    }
    let mut out = format!("\n  refused ({}):", failed.len());
    for (jid, code) in failed {
        out.push_str(&format!("\n    {jid}: error {code}"));
    }
    out
}

/// Resolve the chat a per-chat action (archive, mute, clear, star) targets.
///
/// A group is named with `group_jid`, a person with `phone`. The group branch
/// goes through `resolve_group_jid` so a group id can never be read as a phone
/// number; everything else takes the normal `resolve_jid` path (explicit
/// `phone`, else the session chat, else the owner).
#[allow(clippy::result_large_err)]
pub(crate) async fn resolve_chat_jid(
    input: &Value,
    whatsapp_state: &WhatsAppState,
    config_rx: &tokio::sync::watch::Receiver<Config>,
    session_id: uuid::Uuid,
) -> std::result::Result<(Jid, String), ToolResult> {
    if input.get("group_jid").and_then(|v| v.as_str()).is_some() {
        let jid = resolve_group_jid(input)?;
        let jid_str = jid.to_string();
        Ok((jid, jid_str))
    } else {
        resolve_jid(input, whatsapp_state, config_rx, session_id).await
    }
}

/// Parse the `participants` array into JIDs.
///
/// Each entry is either a full JID or a bare phone number, which is wrapped as
/// `<number>@s.whatsapp.net`. A `:device` suffix is dropped: it names a
/// companion device, not a different person.
#[allow(clippy::result_large_err)]
pub(crate) fn resolve_participants(input: &Value) -> std::result::Result<Vec<Jid>, ToolResult> {
    resolve_jid_list(input, "participants")
}

/// Parse the `contacts` array into JIDs for the read-only contact queries.
///
/// Same normalization as `resolve_participants`, keyed on `contacts` because
/// these actions ask ABOUT people rather than move them. An empty list is
/// still a refusal: a query that names nobody has no answer to report, and
/// defaulting it to "everyone" would fan the lookup out across the account.
#[allow(clippy::result_large_err)]
pub(crate) fn resolve_contacts(input: &Value) -> std::result::Result<Vec<Jid>, ToolResult> {
    resolve_jid_list(input, "contacts")
}

/// Normalize one contact spelling into a JID.
///
/// Accepts a bare phone number (with an optional leading `+` and `:device`
/// suffix) or a full JID. Shared by the array parsers and by the single
/// optional `participant`, so a spelling cannot mean one thing in a list and
/// another thing on its own.
#[allow(clippy::result_large_err)]
fn parse_contact_jid(raw: &str, key: &str) -> std::result::Result<Jid, ToolResult> {
    let raw = raw.trim().trim_start_matches('+');
    if raw.is_empty() {
        return Err(ToolResult::error(format!(
            "'{key}' contains an empty entry."
        )));
    }
    let jid_str = if raw.contains('@') {
        raw.to_string()
    } else {
        let user = raw.split(':').next().unwrap_or(raw);
        format!("{user}@s.whatsapp.net")
    };
    jid_str
        .parse()
        .map_err(|e| ToolResult::error(format!("Invalid '{key}' entry '{raw}': {e}")))
}

/// Parse the optional `participant` parameter into a JID.
///
/// Absent means "no participant", which is the right shape for a direct message
/// or for a message this account sent. Present but unusable is an error rather
/// than a silent `None`: on a group message from someone else the participant is
/// part of the message key, so dropping it would address a different message
/// than the caller named.
#[allow(clippy::result_large_err)]
pub(crate) fn resolve_optional_participant(
    input: &Value,
) -> std::result::Result<Option<Jid>, ToolResult> {
    match input.get("participant") {
        None => Ok(None),
        Some(value) if value.is_null() => Ok(None),
        Some(value) => {
            let raw = value.as_str().ok_or_else(|| {
                ToolResult::error("'participant' must be a phone number or JID string.".to_string())
            })?;
            Ok(Some(parse_contact_jid(raw, "participant")?))
        }
    }
}

/// Parse and validate the `poll_options` array.
///
/// Shared by `send_poll` and `create_quiz` so the two cannot drift apart on
/// what counts as a usable poll: WhatsApp itself accepts 2 to 12 options, and
/// a single-option poll has nothing to choose between.
#[allow(clippy::result_large_err)]
fn poll_options(input: &Value) -> std::result::Result<Vec<String>, ToolResult> {
    let Some(arr) = input.get("poll_options").and_then(|v| v.as_array()) else {
        return Err(ToolResult::error(
            "Missing required 'poll_options' parameter.".to_string(),
        ));
    };
    let opts: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    if opts.len() < 2 {
        return Err(ToolResult::error(
            "'poll_options' must have at least 2 options.".to_string(),
        ));
    }
    if opts.len() > 12 {
        return Err(ToolResult::error(
            "'poll_options' supports a maximum of 12 options.".to_string(),
        ));
    }
    Ok(opts)
}

/// Parse a named array parameter into plain strings.
///
/// The poll actions name things (option labels, not JIDs), so they cannot
/// borrow the JID parsers. An empty list is a refusal for the same reason a
/// missing one is: an action that names nothing has nothing to act on.
#[allow(clippy::result_large_err)]
fn string_list(input: &Value, key: &str) -> std::result::Result<Vec<String>, ToolResult> {
    let Some(arr) = input.get(key).and_then(|v| v.as_array()) else {
        return Err(ToolResult::error(format!(
            "Missing required parameter '{key}' (an array of strings)."
        )));
    };
    let items: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    if items.is_empty() {
        return Err(ToolResult::error(format!(
            "'{key}' must hold at least one entry."
        )));
    }
    Ok(items)
}

/// Decode a base64 string parameter into bytes.
#[allow(clippy::result_large_err)]
fn decode_b64(input: &Value, key: &str) -> std::result::Result<Vec<u8>, ToolResult> {
    use base64::Engine as _;
    let raw = get_str(input, key)?;
    base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .map_err(|e| ToolResult::error(format!("'{key}' is not valid base64: {e}")))
}

/// A non-negative integer parameter, defaulting to `0` when it is absent.
///
/// `0` is the documented off switch for both the resend limiter and the
/// retry bucket, so a missing value lands on "off" rather than on an
/// arbitrary budget the caller never asked for.
fn u32_param(input: &Value, key: &str) -> u32 {
    input.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as u32
}

/// The JID of the account that authored a poll.
///
/// `vote` and `decrypt_vote` both key their crypto on the creator, so it has
/// to be the poll's real author and not whoever is talking now. Omitted means
/// this account, which is the only case the channel has persisted a secret for.
#[allow(clippy::result_large_err)]
fn poll_creator(
    client: &whatsapp_rust::client::Client,
    input: &Value,
) -> std::result::Result<Jid, ToolResult> {
    match input.get("poll_creator_jid").and_then(|v| v.as_str()) {
        Some(raw) => raw
            .parse()
            .map_err(|e| ToolResult::error(format!("'poll_creator_jid' is not a JID: {e}"))),
        None => client.pn().ok_or_else(|| {
            ToolResult::error(
                "Not logged in, so the poll's author cannot be assumed to be this account; \
                 pass 'poll_creator_jid'."
                    .to_string(),
            )
        }),
    }
}

/// The poll's message secret: the caller's, or the one already stored.
///
/// A poll's votes are encrypted against a secret minted when the poll was
/// created, and the channel persists it for every poll this account sent, so
/// the common case needs no input at all. `poll_secret` covers a poll authored
/// elsewhere. Missing on both sides is an error rather than an empty key: the
/// lib would fail on a zero-length secret anyway, and saying why is cheaper
/// than decoding a crypto error.
#[allow(clippy::result_large_err)]
async fn resolve_poll_secret(
    client: &whatsapp_rust::client::Client,
    input: &Value,
    chat: &str,
    creator: &str,
    poll_id: &str,
) -> std::result::Result<Vec<u8>, ToolResult> {
    if input.get("poll_secret").and_then(|v| v.as_str()).is_some() {
        return decode_b64(input, "poll_secret");
    }
    match client
        .persistence_manager()
        .backend()
        .get_msg_secret(chat, creator, poll_id)
        .await
    {
        Ok(Some(secret)) => Ok(secret),
        Ok(None) => Err(ToolResult::error(format!(
            "No message secret is stored for poll {poll_id} in {chat}. Pass 'poll_secret' \
             (base64) if you have it; without it a vote cannot be encrypted or decrypted."
        ))),
        Err(e) => Err(ToolResult::error(format!(
            "Could not read the poll's message secret: {e}"
        ))),
    }
}

/// Parse a named array parameter into JIDs.
///
/// Shared by `resolve_participants` and `resolve_contacts` so the two spellings
/// cannot drift apart on normalization.
#[allow(clippy::result_large_err)]
fn resolve_jid_list(input: &Value, key: &str) -> std::result::Result<Vec<Jid>, ToolResult> {
    let Some(raw_list) = input.get(key).and_then(|v| v.as_array()) else {
        return Err(ToolResult::error(format!(
            "Missing required parameter '{key}' (an array of phone numbers or JIDs)."
        )));
    };
    if raw_list.is_empty() {
        return Err(ToolResult::error(format!(
            "'{key}' must name at least one person."
        )));
    }
    let mut jids = Vec::with_capacity(raw_list.len());
    for entry in raw_list {
        jids.push(parse_contact_jid(entry.as_str().unwrap_or(""), key)?);
    }
    Ok(jids)
}

/// Parse the `group_jids` array into group JIDs.
///
/// Each entry may be a full `@g.us` JID or a bare group id, the same spellings
/// `resolve_group_jid` accepts for a single group, and each one passes the same
/// send-scope check.
#[allow(clippy::result_large_err)]
pub(crate) fn resolve_group_jids(input: &Value) -> std::result::Result<Vec<Jid>, ToolResult> {
    let Some(raw_list) = input.get("group_jids").and_then(|v| v.as_array()) else {
        return Err(ToolResult::error(
            "Missing required parameter 'group_jids' (an array of group JIDs).".to_string(),
        ));
    };
    if raw_list.is_empty() {
        return Err(ToolResult::error(
            "'group_jids' must name at least one group.".to_string(),
        ));
    }
    let mut jids = Vec::with_capacity(raw_list.len());
    for entry in raw_list {
        let raw = entry.as_str().unwrap_or("").trim();
        if raw.is_empty() {
            return Err(ToolResult::error(
                "'group_jids' contains an empty entry.".to_string(),
            ));
        }
        let jid: Jid = if raw.contains('@') {
            raw.parse()
                .map_err(|e| ToolResult::error(format!("Invalid group JID '{raw}': {e}")))?
        } else {
            Jid::group(raw)
        };
        if jid.server != Server::Group {
            return Err(ToolResult::error(format!(
                "'{raw}' is not a group JID (expected an @g.us address or a bare group id)."
            )));
        }
        let jid_str = jid.to_string();
        if !crate::cron::send_scope::may_send("whatsapp", &jid_str) {
            let reason = crate::cron::send_scope::refusal_for("whatsapp", &jid_str);
            tracing::warn!("whatsapp_send: {reason}");
            return Err(ToolResult::error(reason));
        }
        jids.push(jid);
    }
    Ok(jids)
}

/// One line per participant change, naming the ones the server refused.
///
/// `add_participants` and friends answer per participant, and a partial result
/// is the common case: a number that is not on WhatsApp, a member the caller
/// may not promote. Reporting a bare "done" would hide which entries did not
/// land, so the refused ones are listed with the server's error code.
pub(crate) fn summarize_participant_changes(changes: &[ParticipantChangeResponse]) -> String {
    let ok = changes.iter().filter(|c| c.is_ok()).count();
    let mut out = format!("{ok}/{} succeeded", changes.len());
    for change in changes.iter().filter(|c| !c.is_ok()) {
        out.push_str(&format!(
            "\n  {}: {}",
            change.jid,
            change.error.as_deref().unwrap_or("no error code")
        ));
    }
    out
}

/// Prefix outgoing text with the channel attribution header. Shared by the
/// send and reply paths so persisted history carries ONE consistent format
/// for every agent-authored message (#1490-E: reply persisted bare text
/// while send persisted the tagged form).
pub(crate) fn tag_with_header(message: &str) -> String {
    format!(
        "{}\n\n{}",
        crate::channels::whatsapp::handler::MSG_HEADER,
        message
    )
}

/// Concatenate the chunks that actually left. `split_message` returns
/// contiguous slices of its input, so the concat is the exact delivered
/// prefix of the tagged text (#1490-B).
pub(crate) fn delivered_prefix(delivered: &[&str]) -> String {
    delivered.concat()
}

/// Error text for a chunked send that died partway: names how many chunks
/// were delivered (and persisted) vs which one failed, and tells the model
/// what a retry may and must-not resend (#1490-B; blind retries used to
/// duplicate chunk 1).
pub(crate) fn partial_failure_report(total: usize, delivered: usize, err: &str) -> String {
    if delivered == 0 {
        format!(
            "Failed to send WhatsApp message: {err}. No chunks were delivered; safe to retry the whole message."
        )
    } else {
        format!(
            "Failed to send WhatsApp message: chunk {} of {} failed: {err}. Chunks 1-{} were DELIVERED and persisted; do NOT resend them. Retry only the remaining text if needed.",
            delivered + 1,
            total,
            delivered
        )
    }
}

/// Persist outgoing messages to `channel_messages` for reply-recovery.
pub(crate) async fn persist_outgoing(jid: &Jid, content: &str) {
    if content.trim().is_empty() {
        return;
    }
    let Some(pool) = crate::db::global_pool() else {
        return;
    };
    let repo = crate::db::ChannelMessageRepository::new(pool.clone());
    let chat_id = jid.to_string();
    let cm = crate::db::models::ChannelMessage::new(
        "whatsapp".to_string(),
        chat_id,
        None,
        "bot:opencrabs".to_string(),
        "OpenCrabs".to_string(),
        content.to_string(),
        "text".to_string(),
        None,
    );
    if let Err(e) = repo.insert(&cm).await {
        tracing::warn!("whatsapp_send: failed to persist outgoing message: {}", e);
    }
}

/// Read a local file, expanding tilde. Returns (bytes, detected mime, filename).
#[allow(clippy::result_large_err)]
async fn read_local_media(
    path: &str,
    default_mime: &str,
) -> std::result::Result<(Vec<u8>, String, String), ToolResult> {
    let expanded = crate::brain::tools::error::expand_tilde(path);
    let bytes = tokio::fs::read(&expanded).await.map_err(|e| {
        ToolResult::error(format!(
            "Failed to read file '{}': {}",
            expanded.display(),
            e
        ))
    })?;
    let filename = expanded
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let mime = mime_from_extension(&expanded.to_string_lossy())
        .unwrap_or_else(|| default_mime.to_string());
    Ok((bytes, mime, filename))
}

/// Detect MIME type from file extension.
pub(crate) fn mime_from_extension(path: &str) -> Option<String> {
    let ext = path.rsplit('.').next()?.to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "png" => Some("image/png".to_string()),
        "gif" => Some("image/gif".to_string()),
        "webp" => Some("image/webp".to_string()),
        "mp4" => Some("video/mp4".to_string()),
        "3gp" => Some("video/3gpp".to_string()),
        "avi" => Some("video/x-msvideo".to_string()),
        "mov" => Some("video/quicktime".to_string()),
        "mp3" => Some("audio/mpeg".to_string()),
        "ogg" | "opus" => Some("audio/ogg".to_string()),
        "aac" => Some("audio/aac".to_string()),
        "m4a" => Some("audio/mp4".to_string()),
        "pdf" => Some("application/pdf".to_string()),
        "doc" | "docx" => Some("application/msword".to_string()),
        "xls" | "xlsx" => Some("application/vnd.ms-excel".to_string()),
        "ppt" | "pptx" => Some("application/vnd.ms-powerpoint".to_string()),
        "txt" => Some("text/plain".to_string()),
        "csv" => Some("text/csv".to_string()),
        "zip" => Some("application/zip".to_string()),
        _ => None,
    }
}

/// Largest edge of the status thumbnail, in pixels. WhatsApp requires a JPEG
/// thumbnail alongside an image status; a 32x32 JPEG is a couple of hundred
/// bytes and is all the client needs for the notification preview, so the
/// full-resolution bytes go to the CDN untouched.
pub(crate) const STATUS_THUMBNAIL_MAX_EDGE: u32 = 32;

/// Build the JPEG thumbnail a WhatsApp image status must carry.
///
/// Returns `None` when the bytes are not a decodable image (or cannot be
/// re-encoded), which the caller reports instead of posting a status whose
/// thumbnail is missing.
pub(crate) fn make_jpeg_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let thumb = img.thumbnail(STATUS_THUMBNAIL_MAX_EDGE, STATUS_THUMBNAIL_MAX_EDGE);
    let mut out = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Jpeg).ok()?;
    Some(out.into_inner())
}

/// Upload media to WhatsApp servers and return the upload response.
/// Uses the same pattern as the WhatsApp handler.
#[allow(clippy::result_large_err)]
async fn upload_media(
    client: &whatsapp_rust::client::Client,
    data: Vec<u8>,
    media_type: wacore::download::MediaType,
) -> std::result::Result<whatsapp_rust::upload::UploadResponse, ToolResult> {
    client
        .upload(
            data,
            media_type,
            whatsapp_rust::upload::UploadOptions::new(),
        )
        .await
        .map_err(|e| ToolResult::error(format!("Media upload failed: {}", e)))
}

/// Build a vCard string from name and phone number.
pub(crate) fn build_vcard(name: &str, phone: &str) -> String {
    format!(
        "BEGIN:VCARD\nVERSION:3.0\nFN:{}\nTEL;TYPE=CELL:{}\nEND:VCARD",
        name, phone
    )
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Tool for WhatsAppSendTool {
    fn name(&self) -> &str {
        "whatsapp_send"
    }

    fn description(&self) -> &str {
        "Full WhatsApp control: send messages, reply, delete, send photos/documents/audio/video/stickers, \
         locations, contacts, emoji reactions, polls, typing indicators, and mark messages as read. \
         If no phone number is specified, messages go to the owner (primary user). \
         Requires WhatsApp to be connected first."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "send", "reply", "delete",
                        "pin", "unpin", "forward", "set_profile_name", "set_profile_status",
                        "set_profile_picture", "remove_profile_picture",
                        "send_photo", "send_document", "send_audio", "send_video", "send_sticker",
                        "send_location", "send_contact",
                        "react", "send_poll", "edit",
                        "typing", "mark_read",
                        "block_contact", "unblock_contact", "list_blocked",
                        "send_buttons",
                        "status_update", "list_newsletters",
                        "create_label", "assign_label",
                        "create_group", "leave_group", "set_group_subject",
                        "set_group_description", "add_participants", "remove_participants",
                        "promote_participants", "demote_participants", "get_group_metadata",
                        "get_invite_link", "join_with_invite_code",
                        "set_announce", "set_locked", "set_ephemeral",
                        "set_membership_approval", "set_member_add_mode",
                        "set_limit_sharing", "set_group_history",
                        "set_no_frequently_forwarded", "set_allow_admin_reports",
                        "get_membership_requests", "approve_membership_requests",
                        "reject_membership_requests", "cancel_membership_requests",
                        "get_participating", "is_admin",
                        "get_profile_picture", "get_group_profile_picture",
                        "is_on_whatsapp", "get_user_info",
                        "archive_chat", "unarchive_chat", "mute_chat", "unmute_chat",
                        "star_message", "unstar_message", "clear_chat", "save_contact",
                        "set_available", "set_unavailable", "subscribe_presence",
                        "unsubscribe_presence",
                        "create_quiz", "vote", "decrypt_vote", "aggregate_votes",
                        "newsletter_get_metadata", "newsletter_get_metadata_by_invite",
                        "newsletter_join", "newsletter_leave",
                        "newsletter_create", "newsletter_update",
                        "newsletter_revoke_message", "newsletter_get_messages",
                        "community_create", "community_deactivate",
                        "community_create_subgroup", "community_link_subgroups",
                        "community_unlink_subgroups", "community_join_subgroup",
                        "community_remove_participants", "community_get_participating",
                        "community_get_subgroups", "community_get_subgroup_participant_counts",
                        "community_query_linked_group",
                        "community_get_linked_groups_participants",
                        "create_event", "respond_event", "post_comment", "wa_introspect"
                    ],
                    "description": "The WhatsApp action to perform."
                },
                "message": {
                    "type": "string",
                    "description": "Message text (send, reply, caption for media); the new name or status text for set_profile_name / set_profile_status. For status_update: the text status, or the caption when 'media_path' is given."
                },
                "phone": {
                    "type": "string",
                    "description": "Phone number in E.164 format (e.g. '+15551234567'). For the chat actions (archive_chat, unarchive_chat, mute_chat, unmute_chat, clear_chat, star_message, unstar_message) it names the chat to act on; for save_contact it is required, and for subscribe_presence / unsubscribe_presence it names the contact whose updates to watch. Omit to message the owner."
                },
                "message_id": {
                    "type": "string",
                    "description": "WhatsApp message ID for reply, delete, edit, react, mark_read, forward, star_message, unstar_message"
                },
                "stanza_id": {
                    "type": "string",
                    "description": "For edit only: pin the outer stanza id to an existing message id so clients re-render that slot. Best-effort and server dependent, and the edit then skips id-keyed local state. Omit for a fresh stanza id, which is the safe default."
                },
                "from_me": {
                    "type": "boolean",
                    "description": "Whether the target message was sent by us (for react/mark_read/star_message/unstar_message). Default false."
                },
                "participant": {
                    "type": "string",
                    "description": "For react only: the JID of the original sender of a group or status message being reacted to. Required for a group reaction to be attributed; omit for a direct message."
                },
                "media_path": {
                    "type": "string",
                    "description": "Local file path for media (send_photo, send_document, send_audio, send_video, send_sticker, and the JPEG for set_profile_picture). For status_update: a local image file to post as an image status instead of a text status, in which case 'message' becomes its caption."
                },
                "multi_select": {
                    "type": "boolean",
                    "description": "For send_poll: true lets a voter pick several options. Default false (single choice)."
                },
                "voice_note": {
                    "type": "boolean",
                    "description": "For send_audio: true (default) renders a native WhatsApp voice note (tap-to-play bubble) and shows a recording indicator while uploading. false sends a plain audio file attachment."
                },
                "buttons": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "For send_buttons: up to 3 button labels. WhatsApp draws at most 3; extras are dropped rather than silently ignored on the wire."
                },
                "footer": {
                    "type": "string",
                    "description": "For send_buttons: small text under the buttons."
                },
                "targets": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "For status_update: phone numbers to post the status to. Every one must appear in channels.whatsapp.broadcast.allowed_targets or NOTHING is sent. Omit to use the whole allowlist."
                },
                "label_id": {
                    "type": "string",
                    "description": "For create_label (omit to make a new label; supplying an existing id renames/recolors it) and assign_label (required)"
                },
                "label_color": {
                    "type": "integer",
                    "description": "For create_label: WhatsApp colour index. Default 0."
                },
                "remove": {
                    "type": "boolean",
                    "description": "For assign_label: true removes the label from the chat instead of adding it."
                },
                "caption": {
                    "type": "string",
                    "description": "Caption for media messages (send_photo, send_video, send_document). status_update takes its caption from 'message'."
                },
                "latitude": {
                    "type": "number",
                    "description": "Latitude for send_location"
                },
                "longitude": {
                    "type": "number",
                    "description": "Longitude for send_location"
                },
                "location_name": {
                    "type": "string",
                    "description": "Optional location name for send_location"
                },
                "location_address": {
                    "type": "string",
                    "description": "Optional location address for send_location"
                },
                "contact_name": {
                    "type": "string",
                    "description": "Contact display name for send_contact"
                },
                "contact_phone": {
                    "type": "string",
                    "description": "Contact phone number for send_contact"
                },
                "emoji": {
                    "type": "string",
                    "description": "Emoji for react action (e.g. '👍', '❤️'). Empty string to remove reaction."
                },
                "poll_question": {
                    "type": "string",
                    "description": "Poll question text for send_poll and create_quiz"
                },
                "poll_options": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Array of poll option strings (2-12) for send_poll and create_quiz. For aggregate_votes it is optional, and only fills in the option order for a poll whose own list has already aged out."
                },
                "typing_active": {
                    "type": "boolean",
                    "description": "For typing action: true = composing, false = paused. Default true."
                },
                "ephemeral": {
                    "type": "integer",
                    "description": "For send: disappearing-message TTL in seconds (86400 = 24h, 604800 = 7d, 7776000 = 90d max). 0 means the message does not expire, overriding the channel default. Omit to use the channel default."
                },
                "group_jid": {
                    "type": "string",
                    "description": "Target group JID for the group and community actions (e.g. '120363012345678901@g.us'). A bare group id is accepted and wrapped as a group JID. For the community_* actions it names the community itself; a group inside it is named with 'subgroup_jid'. Required by every action that names an existing group."
                },
                "participants": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Phone numbers (E.164, leading '+' optional) or JIDs. Used by create_group, add_participants, remove_participants, promote_participants, demote_participants, approve_membership_requests, reject_membership_requests, cancel_membership_requests and is_admin."
                },
                "contacts": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Phone numbers (E.164, leading '+' optional) or JIDs to look up. Used by is_on_whatsapp and get_user_info."
                },
                "group_jids": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Group JIDs. Used by get_group_profile_picture (max 1000) and by community_link_subgroups / community_unlink_subgroups to name the groups to link or unlink."
                },
                "preview": {
                    "type": "boolean",
                    "description": "For get_profile_picture and get_group_profile_picture: true returns the small preview thumbnail, false (the default) the full-size picture."
                },
                "participant": {
                    "type": "string",
                    "description": "For star_message and unstar_message: the phone number or JID of the sender whose message is being starred, required when the message is a group message from someone else. Omit for a DM or for your own message."
                },
                "mute_until_ms": {
                    "type": "integer",
                    "description": "For mute_chat: a Unix epoch timestamp in milliseconds to mute until. Omit to mute indefinitely."
                },
                "delete_starred": {
                    "type": "boolean",
                    "description": "For clear_chat: also remove the starred messages in the chat. Default false."
                },
                "delete_media": {
                    "type": "boolean",
                    "description": "For clear_chat: also remove downloaded media. Default false."
                },
                "full_name": {
                    "type": "string",
                    "description": "For save_contact: the contact's full name."
                },
                "first_name": {
                    "type": "string",
                    "description": "For save_contact: the contact's short name. Omitted when not given, rather than derived from full_name."
                },
                "save_on_primary_addressbook": {
                    "type": "boolean",
                    "description": "For save_contact: also save to the phone's own address book. Default true."
                },
                "quiz_correct_index": {
                    "type": "integer",
                    "description": "For create_quiz: the 0-based index into 'poll_options' of the correct answer. Required, because option 0 is a real answer and a missing index must not read as one."
                },
                "poll_msg_id": {
                    "type": "string",
                    "description": "The poll's WhatsApp message ID, for vote, decrypt_vote and aggregate_votes. It is the key every vote is filed under."
                },
                "option_names": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "For vote: the option labels to select, spelled exactly as they appear in the poll. A label the poll does not carry is refused by the lib rather than silently dropped."
                },
                "poll_creator_jid": {
                    "type": "string",
                    "description": "For vote and decrypt_vote: the JID of the account that created the poll. Omit when this account created it, which is the only case whose message secret is stored locally."
                },
                "poll_secret": {
                    "type": "string",
                    "description": "For vote and decrypt_vote: the poll's message secret, base64-encoded. Omit to use the secret already stored for a poll this account authored."
                },
                "voter_jid": {
                    "type": "string",
                    "description": "For decrypt_vote: the phone number or JID of the voter whose ciphertext this is. It keys the vote's encryption together with the creator, so the wrong one does not open."
                },
                "vote_payload": {
                    "type": "string",
                    "description": "For decrypt_vote: the base64 'enc_payload' of a raw poll vote."
                },
                "vote_iv": {
                    "type": "string",
                    "description": "For decrypt_vote: the base64 'enc_iv' of a raw poll vote."
                },
                "subject": {
                    "type": "string",
                    "description": "New group subject for create_group and set_group_subject."
                },
                "description": {
                    "type": "string",
                    "description": "New group description for set_group_description, or the new channel description for newsletter_create / newsletter_update. An empty string deletes the description."
                },
                "invite_code": {
                    "type": "string",
                    "description": "Invite code (the part after 'chat.whatsapp.com/') for join_with_invite_code."
                },
                "reset_invite": {
                    "type": "boolean",
                    "description": "For get_invite_link: true revokes the current link and returns a fresh one. Default false (the existing link)."
                },
                "announce": {
                    "type": "boolean",
                    "description": "For set_announce: true makes the group announcement-only (only admins may send), false opens it to every member."
                },
                "locked": {
                    "type": "boolean",
                    "description": "For set_locked: true restricts group-info edits to admins, false lets every member edit."
                },
                "expiration": {
                    "type": "integer",
                    "description": "For set_ephemeral: disappearing-message timer in seconds (86400 = 24h, 604800 = 7d, 7776000 = 90d). 0 disables the timer."
                },
                "membership_approval": {
                    "type": "string",
                    "enum": ["off", "on"],
                    "description": "For set_membership_approval: 'on' requires an admin to approve each join request, 'off' lets anyone with the link in."
                },
                "member_add_mode": {
                    "type": "string",
                    "enum": ["admin_add", "all_member_add"],
                    "description": "For set_member_add_mode: 'admin_add' lets only admins add members, 'all_member_add' lets any member add them."
                },
                "limit_sharing": {
                    "type": "boolean",
                    "description": "For set_limit_sharing: true restricts how group content may be forwarded or saved."
                },
                "group_history": {
                    "type": "boolean",
                    "description": "For set_group_history: true shares recent history with members as they join."
                },
                "no_frequently_forwarded": {
                    "type": "boolean",
                    "description": "For set_no_frequently_forwarded: true stops members from forwarding messages already marked as frequently forwarded."
                },
                "allow_admin_reports": {
                    "type": "boolean",
                    "description": "For set_allow_admin_reports: true lets members report messages to the group admins."
                },
                "newsletter_jid": {
                    "type": "string",
                    "description": "Target newsletter (channel) for the newsletter_* actions, e.g. '1234@newsletter'. A bare channel id is accepted."
                },
                "count": {
                    "type": "integer",
                    "description": "For newsletter_get_messages: how many posts to fetch, newest first. Default 20, capped at 100."
                },
                "before": {
                    "type": "integer",
                    "description": "For newsletter_get_messages: fetch posts older than this server_id, the pagination cursor from a previous page."
                },
                "event_msg_id": {
                    "type": "string",
                    "description": "For respond_event: the id of the event message being answered, as returned by create_event."
                },
                "event_creator_jid": {
                    "type": "string",
                    "description": "For respond_event: the JID of the account that created the event. The RSVP key is derived from it."
                },
                "event_secret": {
                    "type": "string",
                    "description": "For respond_event: the event's message secret, base64-encoded, as returned by create_event."
                },
                "event_response": {
                    "type": "string",
                    "enum": ["going", "not_going", "maybe"],
                    "description": "For respond_event: the RSVP answer."
                },
                "extra_guest_count": {
                    "type": "integer",
                    "description": "For respond_event: how many extra guests the responder is bringing."
                },
                "parent_msg_id": {
                    "type": "string",
                    "description": "For post_comment: the id of the channel post being commented on."
                },
                "parent_participant": {
                    "type": "string",
                    "description": "For post_comment: the JID of the account that wrote the post. Receivers key the comment decryption on it."
                },
                "start_time": {
                    "type": "integer",
                    "description": "For create_event: event start time in milliseconds since the epoch."
                },
                "end_time": {
                    "type": "integer",
                    "description": "For create_event: event end time in milliseconds since the epoch."
                },
                "debug_op": {
                    "type": "string",
                    "enum": ["stats", "wait_for_node", "wait_for_sent_node", "acquire_raw_node", "release_raw_node", "set_resend_rate_limit", "set_skip_history_sync", "set_wanted_pre_key_count", "set_retry_admission"],
                    "description": "For wa_introspect: which runtime call to make against the live session. Read-only ops are 'stats', 'wait_for_node' and 'wait_for_sent_node'; the rest change the running client."
                },
                "node_tag": {
                    "type": "string",
                    "description": "For wa_introspect wait_for_node/wait_for_sent_node: the stanza tag to match, e.g. 'notification' or 'message'."
                },
                "node_attr_key": {
                    "type": "string",
                    "description": "For wa_introspect wait_for_node/wait_for_sent_node: narrows the match to one attribute, e.g. 'type'. Set together with node_attr_value."
                },
                "node_attr_value": {
                    "type": "string",
                    "description": "For wa_introspect wait_for_node/wait_for_sent_node: the attribute value to match, e.g. 'w:gp2'."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "For wa_introspect wait_for_node/wait_for_sent_node: how long to wait before reporting that nothing arrived. Default 10000, clamped to 100..60000."
                },
                "burst": {
                    "type": "integer",
                    "description": "For wa_introspect set_resend_rate_limit/set_retry_admission: the instantaneous per-chat allowance. 0 disables the limiter."
                },
                "refill_per_min": {
                    "type": "integer",
                    "description": "For wa_introspect set_resend_rate_limit/set_retry_admission: the sustained per-chat allowance, refilled every minute."
                },
                "debug_enabled": {
                    "type": "boolean",
                    "description": "For wa_introspect set_skip_history_sync: true acknowledges history-sync notifications without downloading them."
                },
                "pre_key_count": {
                    "type": "integer",
                    "description": "For wa_introspect set_wanted_pre_key_count: how many one-time pre-keys to generate per upload batch. Takes effect on the next upload."
                },
                "subgroup_jid": {
                    "type": "string",
                    "description": "For community_query_linked_group and community_join_subgroup: the linked group inside the community (e.g. '120363012345678901@g.us'). A bare group id is accepted. The community itself is named with 'group_jid'."
                },
                "closed": {
                    "type": "boolean",
                    "description": "For community_create: true makes the community closed, so joining it requires approval. Default false."
                },
                "allow_non_admin_sub_group_creation": {
                    "type": "boolean",
                    "description": "For community_create: true lets members who are not admins create subgroups. Default false."
                },
                "create_general_chat": {
                    "type": "boolean",
                    "description": "For community_create: true also creates the community's general chat subgroup. Default true."
                },
                "remove_orphan_members": {
                    "type": "boolean",
                    "description": "For community_unlink_subgroups: true also removes members who belong only to the groups being unlinked. Default false."
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

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let action = match input.get("action").and_then(|v| v.as_str()) {
            Some(a) if !a.is_empty() => a.to_string(),
            _ => {
                return Ok(ToolResult::error(
                    "Missing required 'action' parameter.".to_string(),
                ));
            }
        };

        let client = pget!(get_client(&self.whatsapp_state).await);

        match action.as_str() {
            // ── send ─────────────────────────────────────────────────────────
            "send" => {
                let message = match input.get("message").and_then(|v| v.as_str()) {
                    Some(m) if !m.is_empty() => m.to_string(),
                    _ => {
                        return Ok(ToolResult::error(
                            "Missing or empty 'message' parameter.".to_string(),
                        ));
                    }
                };
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );

                // Convert markdown to WhatsApp format and prepend agent header
                let message = crate::utils::slack_fmt::markdown_to_mrkdwn(&message);
                let tagged = tag_with_header(&message);
                let chunks = crate::channels::whatsapp::handler::split_message(&tagged, 4000);
                let total = chunks.len();
                let mut delivered: Vec<&str> = Vec::with_capacity(total);
                // #1407: pace sends through the shared limiter; over-cap
                // chunks park FIFO for the drainer (queued text must NOT
                // be resent by the model). Owner-bound sends bypass.
                let (rl_cfg, rl_owner) = {
                    let wa = &self.config_rx.borrow().channels.whatsapp;
                    (
                        wa.rate_limit.clone(),
                        wa.is_owner(jid_str.split('@').next().unwrap_or(&jid_str)),
                    )
                };
                // #1487: disappearing messages. An explicit `ephemeral: 0`
                // means "do not expire" and overrides the channel default,
                // so this is not a plain `or`.
                let ttl = crate::channels::whatsapp::ephemeral::resolve(
                    input.get("ephemeral").and_then(|v| v.as_u64()),
                    self.config_rx.borrow().channels.whatsapp.ephemeral_ttl,
                );
                let send_opts = match ttl {
                    Some(seconds) => whatsapp_rust::send::SendOptions::default()
                        .with_ephemeral_expiration(seconds),
                    None => whatsapp_rust::send::SendOptions::default(),
                };
                let mut queued_any = false;
                for chunk in chunks {
                    if let GateOutcome::Queued { .. } = self
                        .whatsapp_state
                        .rate_limiter
                        .gate(&rl_cfg, &jid_str, chunk, rl_owner)
                        .await
                    {
                        queued_any = true;
                        continue;
                    }
                    let wa_msg = waproto::whatsapp::Message {
                        conversation: Some(chunk.to_string()),
                        ..Default::default()
                    };
                    if let Err(e) = client
                        .send_message_with_options(jid.clone(), wa_msg, send_opts.clone())
                        .await
                    {
                        // #1490-B: account for what already left the building.
                        // Delivered chunks are persisted so history matches
                        // reality, and the error names exactly what arrived vs
                        // what failed so a retry cannot duplicate chunk 1.
                        let prefix = delivered_prefix(&delivered);
                        if !prefix.trim().is_empty() {
                            persist_outgoing(&jid, &prefix).await;
                        }
                        return Ok(ToolResult::error(partial_failure_report(
                            total,
                            delivered.len(),
                            &e.to_string(),
                        )));
                    }
                    delivered.push(chunk);
                }

                if queued_any {
                    // Queued chunks flush via the drainer (which persists
                    // them); record only what actually left now.
                    if !delivered.is_empty() {
                        let prefix = delivered_prefix(&delivered);
                        if !prefix.trim().is_empty() {
                            persist_outgoing(&jid, &prefix).await;
                        }
                    }
                    return Ok(ToolResult::success(format!(
                        "WhatsApp daily cap reached: {} of {} chunk(s) queued for automatic delivery as the 24h window slides (owner alerted once); {} delivered now. Do NOT resend the queued part.",
                        total - delivered.len(),
                        total,
                        delivered.len()
                    )));
                }
                persist_outgoing(&jid, &tagged).await;
                Ok(ToolResult::success(match ttl {
                    Some(seconds) => format!(
                        "Message sent to {} via WhatsApp; it disappears after {}.",
                        jid_str,
                        crate::channels::whatsapp::ephemeral::describe(seconds)
                    ),
                    None => format!("Message sent to {} via WhatsApp.", jid_str),
                }))
            }

            // ── reply ────────────────────────────────────────────────────────
            "reply" => {
                let message = pget!(get_str(&input, "message")).to_string();
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let msg_id = pget!(get_str(&input, "message_id")).to_string();

                let formatted = crate::utils::slack_fmt::markdown_to_mrkdwn(&message);
                // #1490-E: the reply carries and persists the same tagged form
                // as the send path - one event class, one format.
                let tagged = tag_with_header(&formatted);
                // #1490-C: chunk the reply like send does. WhatsApp rejects
                // or truncates over-long message text, so a long reply must
                // not ride a single ExtendedTextMessage. Chunk 0 keeps the
                // quote context; later chunks go as plain conversation
                // messages (one quoted reply plus N-1 normal follow-ups -
                // quoting the same stanza N times is noise).
                let chunks = crate::channels::whatsapp::handler::split_message(&tagged, 4000);
                let total = chunks.len();
                let mut delivered: Vec<&str> = Vec::with_capacity(total);
                // #1407: same limiter gating as the send arm. Queued reply
                // chunks lose their quote context on drainer flush
                // (documented tradeoff); ordering is preserved FIFO.
                let (rl_cfg, rl_owner) = {
                    let wa = &self.config_rx.borrow().channels.whatsapp;
                    (
                        wa.rate_limit.clone(),
                        wa.is_owner(jid_str.split('@').next().unwrap_or(&jid_str)),
                    )
                };
                let mut queued_any = false;
                for (i, chunk) in chunks.into_iter().enumerate() {
                    if let GateOutcome::Queued { .. } = self
                        .whatsapp_state
                        .rate_limiter
                        .gate(&rl_cfg, &jid_str, chunk, rl_owner)
                        .await
                    {
                        queued_any = true;
                        continue;
                    }
                    let wa_msg = if i == 0 {
                        // Only the lead chunk quotes the original message.
                        let quoted = waproto::whatsapp::message::ExtendedTextMessage {
                            text: Some(chunk.to_string()),
                            context_info: waproto::whatsapp::ContextInfo {
                                stanza_id: Some(msg_id.clone()),
                                remote_jid: Some(jid_str.clone()),
                                ..Default::default()
                            }
                            .into(),
                            ..Default::default()
                        };
                        waproto::whatsapp::Message {
                            extended_text_message: quoted.into(),
                            ..Default::default()
                        }
                    } else {
                        waproto::whatsapp::Message {
                            conversation: Some(chunk.to_string()),
                            ..Default::default()
                        }
                    };
                    if let Err(e) = client.send_message(jid.clone(), wa_msg).await {
                        // Same accounting as the send path (#1490-B).
                        let prefix = delivered_prefix(&delivered);
                        if !prefix.trim().is_empty() {
                            persist_outgoing(&jid, &prefix).await;
                        }
                        return Ok(ToolResult::error(partial_failure_report(
                            total,
                            delivered.len(),
                            &e.to_string(),
                        )));
                    }
                    delivered.push(chunk);
                }

                if queued_any {
                    if !delivered.is_empty() {
                        let prefix = delivered_prefix(&delivered);
                        if !prefix.trim().is_empty() {
                            persist_outgoing(&jid, &prefix).await;
                        }
                    }
                    return Ok(ToolResult::success(format!(
                        "WhatsApp daily cap reached: {} of {} reply chunk(s) queued for automatic delivery as the 24h window slides (owner alerted once); {} delivered now. Do NOT resend the queued part.",
                        total - delivered.len(),
                        total,
                        delivered.len()
                    )));
                }
                persist_outgoing(&jid, &tagged).await;
                Ok(ToolResult::success(format!(
                    "Reply sent to {} via WhatsApp.",
                    jid_str
                )))
            }

            // ── delete ───────────────────────────────────────────────────────
            "delete" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let msg_id = pget!(get_str(&input, "message_id")).to_string();

                match client
                    .revoke_message(jid, msg_id.clone(), Default::default())
                    .await
                {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Message {} deleted for {}.",
                        msg_id, jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to delete message: {}",
                        e
                    ))),
                }
            }

            // ── pin / unpin ──────────────────────────────────────────────────
            // #1484. The lib pins a CHAT, not a message (`pin_chat` /
            // `unpin_chat` in features/chat_actions.rs), so these take a
            // phone and no message id - the issue's "chat_id + message_id"
            // shape does not exist in the protocol.
            "pin" | "unpin" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let pinning = action == "pin";
                let result = if pinning {
                    client.chat_actions().pin_chat(&jid).await
                } else {
                    client.chat_actions().unpin_chat(&jid).await
                };
                match result {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Chat with {} {}.",
                        jid_str,
                        if pinning { "pinned" } else { "unpinned" }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to {} chat {jid_str}: {e}",
                        if pinning { "pin" } else { "unpin" }
                    ))),
                }
            }

            // ── forward ──────────────────────────────────────────────────────
            // #1484. `Client::forward_message` needs the ORIGINAL proto, not an
            // id: it rebuilds the body with the forward flags and relays media
            // from the same CDN blob. The channel remembers the last 200
            // inbound messages for exactly this (see `recent.rs`); anything
            // older is honestly reported as out of the window rather than
            // silently sending nothing.
            "forward" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the forwarded message");
                let msg_id = pget!(get_str(&input, "message_id")).to_string();
                let Some(original) = self.whatsapp_state.recent.get(&msg_id).await else {
                    return Ok(ToolResult::error(format!(
                        "Message {msg_id} is not in the recent-message window, so it cannot be \
                         forwarded. Only messages this session has seen can be forwarded."
                    )));
                };
                match client.forward_message(jid, &original).await {
                    Ok(result) => Ok(ToolResult::success(format!(
                        "Forwarded message {} to {} via WhatsApp (new id {}).",
                        msg_id, jid_str, result.message_id
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to forward {msg_id} to {jid_str}: {e}"
                    ))),
                }
            }

            // ── set_profile_name / set_profile_status ────────────────────────
            // #1484 asked for a separate `whatsapp_profile` tool. Kept as
            // actions here instead: the channel already routes every WhatsApp
            // operation through one tool, and a second one would split that
            // surface for two setters.
            "set_profile_name" => {
                let name = pget!(get_str(&input, "message")).to_string();
                match client.profile().set_push_name(&name).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "WhatsApp profile name set to '{name}'."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set the profile name: {e}"
                    ))),
                }
            }

            "set_profile_status" => {
                let status = pget!(get_str(&input, "message")).to_string();
                match client.profile().set_status_text(&status).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "WhatsApp profile status set to '{status}'."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set the profile status: {e}"
                    ))),
                }
            }

            // ── set_profile_picture / remove_profile_picture ──────────
            "set_profile_picture" => {
                let path = pget!(get_str(&input, "media_path")).to_string();
                let (bytes, mime, _filename) = pget!(read_local_media(&path, "image/jpeg").await);
                // WhatsApp carries only a JPEG here: the bytes go out as the
                // `picture` node payload, and a PNG or a video is rejected on
                // the wire, so the wrong type is refused before the send.
                if mime != "image/jpeg" {
                    return Ok(ToolResult::error(format!(
                        "set_profile_picture needs a JPEG; '{path}' is {mime}. \
                         Convert it first (WhatsApp uses 640x640)."
                    )));
                }
                match client.profile().set_profile_picture(bytes).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "WhatsApp profile picture set from '{path}'."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set the profile picture: {e}"
                    ))),
                }
            }

            "remove_profile_picture" => {
                // The IQ names our own account, so there is no argument to get
                // wrong: the call is the whole arm.
                match client.profile().remove_profile_picture().await {
                    Ok(_) => Ok(ToolResult::success(
                        "WhatsApp profile picture removed.".to_string(),
                    )),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to remove the profile picture: {e}"
                    ))),
                }
            }

            // ── send_photo ───────────────────────────────────────────────────
            "send_photo" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the photo");
                let path = pget!(get_str(&input, "media_path")).to_string();
                let caption = input
                    .get("caption")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let (bytes, mime, _filename) = pget!(read_local_media(&path, "image/jpeg").await);
                let upload =
                    pget!(upload_media(&client, bytes, wacore::download::MediaType::Image).await);

                let wa_msg = waproto::whatsapp::Message {
                    image_message: waproto::whatsapp::message::ImageMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        caption,
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Photo sent to {} via WhatsApp.",
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send photo: {}", e))),
                }
            }

            // ── send_document ────────────────────────────────────────────────
            "send_document" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the document");
                let path = pget!(get_str(&input, "media_path")).to_string();
                let caption = input
                    .get("caption")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let (bytes, mime, filename) =
                    pget!(read_local_media(&path, "application/octet-stream").await);
                let upload = pget!(
                    upload_media(&client, bytes, wacore::download::MediaType::Document).await
                );

                let wa_msg = waproto::whatsapp::Message {
                    document_message: waproto::whatsapp::message::DocumentMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        file_name: Some(filename),
                        caption,
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Document sent to {} via WhatsApp.",
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send document: {}", e))),
                }
            }

            // ── send_audio ───────────────────────────────────────────────────
            "send_audio" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the audio");
                let path = pget!(get_str(&input, "media_path")).to_string();

                let (bytes, mime, _filename) = pget!(read_local_media(&path, "audio/ogg").await);
                // #1486: WhatsApp renders a tap-to-play voice bubble only when
                // the message carries ptt. Without it the same bytes arrive as
                // an unstyled file attachment. Default on, because the caller
                // that wants a plain audio FILE is the rare one, and pass
                // `voice_note: false` to get the old rendering.
                let ptt = crate::channels::whatsapp::voice_note::wants_voice_note(&input);
                let mime = crate::channels::whatsapp::voice_note::voice_note_mimetype(&mime, ptt);
                let upload =
                    pget!(upload_media(&client, bytes, wacore::download::MediaType::Audio).await);

                // Show "recording..." while the upload is in flight, exactly
                // as a human sending a voice note would appear (#1486).
                if ptt && let Err(e) = client.chatstate().send_recording(&jid).await {
                    tracing::warn!(error = %e, "WhatsApp: recording indicator failed");
                }

                let wa_msg = waproto::whatsapp::Message {
                    audio_message: waproto::whatsapp::message::AudioMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        ptt: Some(ptt),
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                let sent = client.send_message(jid.clone(), wa_msg).await;
                // Clear the indicator whether or not the send worked, so the
                // chat is not left showing "recording..." forever.
                if ptt && let Err(e) = client.chatstate().send_paused(&jid).await {
                    tracing::warn!(error = %e, "WhatsApp: clearing recording indicator failed");
                }
                match sent {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "{} sent to {} via WhatsApp.",
                        if ptt { "Voice note" } else { "Audio" },
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send audio: {}", e))),
                }
            }

            // ── send_video ───────────────────────────────────────────────────
            "send_video" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the video");
                let path = pget!(get_str(&input, "media_path")).to_string();
                let caption = input
                    .get("caption")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let (bytes, mime, _filename) = pget!(read_local_media(&path, "video/mp4").await);
                let upload =
                    pget!(upload_media(&client, bytes, wacore::download::MediaType::Video).await);

                let wa_msg = waproto::whatsapp::Message {
                    video_message: waproto::whatsapp::message::VideoMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        caption,
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Video sent to {} via WhatsApp.",
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send video: {}", e))),
                }
            }

            // ── send_sticker ─────────────────────────────────────────────────
            "send_sticker" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the sticker");
                let path = pget!(get_str(&input, "media_path")).to_string();

                let (bytes, mime, _filename) = pget!(read_local_media(&path, "image/webp").await);
                let upload =
                    pget!(upload_media(&client, bytes, wacore::download::MediaType::Sticker).await);

                let wa_msg = waproto::whatsapp::Message {
                    sticker_message: waproto::whatsapp::message::StickerMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Sticker sent to {} via WhatsApp.",
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send sticker: {}", e))),
                }
            }

            // ── send_location ────────────────────────────────────────────────
            "send_location" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the location");
                let lat = match get_f64(&input, "latitude") {
                    Some(v) => v,
                    None => {
                        return Ok(ToolResult::error(
                            "Missing required 'latitude' parameter.".to_string(),
                        ));
                    }
                };
                let lng = match get_f64(&input, "longitude") {
                    Some(v) => v,
                    None => {
                        return Ok(ToolResult::error(
                            "Missing required 'longitude' parameter.".to_string(),
                        ));
                    }
                };
                let name = input
                    .get("location_name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let address = input
                    .get("location_address")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let wa_msg = waproto::whatsapp::Message {
                    location_message: waproto::whatsapp::message::LocationMessage {
                        degrees_latitude: Some(lat),
                        degrees_longitude: Some(lng),
                        name,
                        address,
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Location ({}, {}) sent to {} via WhatsApp.",
                        lat, lng, jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send location: {}", e))),
                }
            }

            // ── send_contact ─────────────────────────────────────────────────
            "send_contact" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the contact card");
                let contact_name = pget!(get_str(&input, "contact_name")).to_string();
                let contact_phone = pget!(get_str(&input, "contact_phone")).to_string();

                let vcard = build_vcard(&contact_name, &contact_phone);
                let wa_msg = waproto::whatsapp::Message {
                    contact_message: waproto::whatsapp::message::ContactMessage {
                        display_name: Some(contact_name.clone()),
                        vcard: Some(vcard),
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Contact '{}' sent to {} via WhatsApp.",
                        contact_name, jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send contact: {}", e))),
                }
            }

            // ── react ────────────────────────────────────────────────────────
            "react" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the reaction");
                let msg_id = pget!(get_str(&input, "message_id")).to_string();
                let emoji = input
                    .get("emoji")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let from_me = input
                    .get("from_me")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let mut message_key = waproto::whatsapp::MessageKey {
                    remote_jid: Some(jid.to_string()),
                    from_me: Some(from_me),
                    id: Some(msg_id.clone()),
                    ..Default::default()
                };
                // #1482-style: the lib owns the reaction envelope. Groups and
                // status messages need the original sender on the key or the
                // receipt cannot be attributed, and a community announcement
                // group needs the target's message secret, which the hand-rolled
                // proto could not supply at all.
                if let Some(p) = input.get("participant").and_then(|v| v.as_str()) {
                    message_key.participant = Some(p.to_string());
                }

                match client.send_reaction(jid, message_key, &emoji).await {
                    Ok(_) => {
                        if emoji.is_empty() {
                            Ok(ToolResult::success(format!(
                                "Reaction removed from message {} for {}.",
                                msg_id, jid_str
                            )))
                        } else {
                            Ok(ToolResult::success(format!(
                                "Reaction '{}' set on message {} for {}.",
                                emoji, msg_id, jid_str
                            )))
                        }
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to send reaction: {}", e))),
                }
            }

            // ── edit ────────────────────────────────────────────────────────────────
            "edit" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the edit");
                let msg_id = pget!(get_str(&input, "message_id")).to_string();
                let new_text = pget!(get_str(&input, "text")).to_string();
                let wa_msg = waproto::whatsapp::Message {
                    conversation: Some(new_text),
                    ..Default::default()
                };
                // The lib owns the edit envelope. Pinning the outer stanza id is
                // best-effort and skips id-keyed state binding, so it is opt-in
                // rather than the default.
                let sent = match input.get("stanza_id").and_then(|v| v.as_str()) {
                    Some(sid) => {
                        client
                            .edit_message_with_options(
                                jid,
                                msg_id.clone(),
                                wa_msg,
                                whatsapp_rust::send::EditOptions::default()
                                    .with_stanza_id(sid.to_string()),
                            )
                            .await
                    }
                    None => client.edit_message(jid, msg_id.clone(), wa_msg).await,
                };
                match sent {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Message {} edited for {}.",
                        msg_id, jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to edit message: {}", e))),
                }
            }

            // ── send_poll ────────────────────────────────────────────────────
            "send_poll" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the poll");
                let question = pget!(get_str(&input, "poll_question")).to_string();
                let opts = pget!(poll_options(&input));

                // #1482: built through the lib rather than by hand. A poll
                // whose MessageContextInfo carries no `message_secret` can
                // never have its votes decrypted - voters derive their
                // encryption key from it - and the hand-rolled version set
                // none, so every vote on a bot poll was unreadable by
                // construction. `polls().create` mints the secret, persists it
                // for the decode path, and picks the proto version WhatsApp
                // Web uses for the select count (v3 single, v1 multi).
                // WhatsApp encodes "how many options a voter may pick".
                // Default 1 (single choice); `multi_select: true` lets a voter
                // pick any number, which is what the count means at its max.
                let selectable = if input
                    .get("multi_select")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    opts.len() as u32
                } else {
                    1
                };
                match client
                    .polls()
                    .create(jid, &question, &opts, selectable)
                    .await
                {
                    Ok((result, _secret)) => {
                        // Votes name their options by hash, never by label, so
                        // a vote can only be read back against the poll's own
                        // option list. Remember it, keyed on the message id.
                        self.whatsapp_state
                            .polls
                            .remember(result.message_id, opts)
                            .await;
                        Ok(ToolResult::success(format!(
                            "Poll '{}' sent to {} via WhatsApp.",
                            question, jid_str
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to send poll: {}", e))),
                }
            }

            // ── typing ───────────────────────────────────────────────────────
            "typing" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let active = input
                    .get("typing_active")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                let result = if active {
                    client.chatstate().send_composing(&jid).await
                } else {
                    client.chatstate().send_paused(&jid).await
                };
                match result {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Typing {} sent to {}.",
                        if active { "composing" } else { "paused" },
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to send typing indicator: {}",
                        e
                    ))),
                }
            }

            // ── mark_read ────────────────────────────────────────────────────
            "mark_read" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let msg_id = pget!(get_str(&input, "message_id")).to_string();

                // Build read receipt node manually (send_protocol_receipt is pub(crate))
                // Receipt goes TO the message sender (jid), not ourselves
                let node = wacore_binary::builder::NodeBuilder::new("receipt")
                    .attrs([
                        ("id", msg_id.clone()),
                        ("type", "read".to_string()),
                        ("to", jid.to_string()),
                    ])
                    .build();
                match client.send_node(node).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Message {} marked as read for {}.",
                        msg_id, jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to mark message as read: {}",
                        e
                    ))),
                }
            }

            // ── send_buttons ─────────────────────────────────────────────────
            // #1411. Exists so the native-flow card can be tried against a
            // real phone WITHOUT handing the approval flow over to it first:
            // that flow is safety-critical, and a prompt that does not render
            // is a prompt the owner cannot answer. Send one of these, look at
            // the screen, and only then decide about `interactive_buttons`.
            "send_buttons" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the button message");
                let body = pget!(get_str(&input, "message")).to_string();
                let labels: Vec<String> = match input.get("buttons").and_then(|v| v.as_array()) {
                    Some(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .filter(|l| !l.trim().is_empty())
                        .collect(),
                    None => Vec::new(),
                };
                if labels.is_empty() {
                    return Ok(ToolResult::error(
                        "send_buttons needs a non-empty 'buttons' array of labels.".to_string(),
                    ));
                }
                let too_many = labels.len() > interactive::MAX_BUTTONS;
                // Ids are derived from the labels so a tap is legible in the
                // logs without the caller having to invent a parallel id
                // scheme for a one-off test message.
                let buttons: Vec<interactive::Button> = labels
                    .iter()
                    .take(interactive::MAX_BUTTONS)
                    .map(|l| {
                        interactive::Button::new(
                            format!("btn_{}", l.trim().to_lowercase().replace(' ', "_")),
                            l.trim(),
                        )
                    })
                    .collect();
                let wa_msg = interactive::build(
                    &body,
                    input.get("footer").and_then(|v| v.as_str()),
                    &buttons,
                );
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Button message sent to {} with {} button(s){}. If the card does not \
                         render on the recipient's phone, that is the #1411 answer: WhatsApp \
                         drops it silently rather than erroring, so a successful send here is \
                         NOT proof it was seen.",
                        jid_str,
                        buttons.len(),
                        if too_many {
                            format!(
                                " ({} dropped; WhatsApp draws at most {})",
                                labels.len() - interactive::MAX_BUTTONS,
                                interactive::MAX_BUTTONS
                            )
                        } else {
                            String::new()
                        }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to send the button message: {e}"
                    ))),
                }
            }

            // ── status_update ────────────────────────────────────────────────
            // #1485. A status goes to a LIST of recipients, so it is the one
            // action here that can reach many people from one call. Both rails
            // apply: every target must be on the configured allowlist, and the
            // sends are paced, so there is no fire-and-forget mass-send path.
            "status_update" => {
                // #1485 follow-up: a status is text OR an image. `message` is
                // therefore optional so it can double as the image caption; a
                // status carrying neither is refused here rather than sending
                // an empty one.
                let text = input
                    .get("message")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let media_path = input
                    .get("media_path")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                if text.is_none() && media_path.is_none() {
                    return Ok(ToolResult::error(
                        "status_update needs 'message' (the text status, or the caption for an \
                         image status) or 'media_path' (a local image file). Nothing was sent."
                            .to_string(),
                    ));
                }
                let (allowlist, pace) = {
                    let wa = &self.config_rx.borrow().channels.whatsapp;
                    (
                        wa.broadcast.allowed_targets.clone(),
                        broadcast::delay(wa.broadcast.min_delay_seconds),
                    )
                };
                let targets: Vec<String> = match input.get("targets").and_then(|v| v.as_array()) {
                    Some(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                    None => allowlist.clone(),
                };
                if targets.is_empty() {
                    return Ok(ToolResult::error(
                        "No status recipients. Set channels.whatsapp.broadcast.allowed_targets \
                         in config.toml, or pass 'targets'. An empty allowlist allows nobody on \
                         purpose."
                            .to_string(),
                    ));
                }
                let (allowed, refused) = broadcast::partition(&targets, &allowlist);
                if !refused.is_empty() {
                    return Ok(ToolResult::error(format!(
                        "{} target(s) are not on channels.whatsapp.broadcast.allowed_targets and \
                         NOTHING was sent: {}. Broadcast is opt-in per number; add them to the \
                         config first.",
                        refused.len(),
                        refused
                            .iter()
                            .map(|t| t.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
                let mut jids = Vec::with_capacity(allowed.len());
                for target in &allowed {
                    match broadcast::target_jid(target) {
                        Some(jid) => jids.push(jid),
                        None => {
                            return Ok(ToolResult::error(format!(
                                "'{target}' is not a usable phone number; nothing was sent."
                            )));
                        }
                    }
                }
                // Pacing is the point: charge the shared budget once per
                // recipient and sleep between them, so a status to N people
                // costs N slots and at least (N-1) * pace seconds.
                for (i, jid) in jids.iter().enumerate() {
                    let jid_str = jid.to_string();
                    gate_send!(self, jid_str, "the status update");
                    if i > 0 {
                        tokio::time::sleep(pace).await;
                    }
                }
                let opts = whatsapp_rust::features::StatusSendOptions::default();
                let posted = match media_path {
                    Some(path) => {
                        let (bytes, mime, _filename) =
                            pget!(read_local_media(&path, "image/jpeg").await);
                        if !mime.starts_with("image/") {
                            return Ok(ToolResult::error(format!(
                                "'{path}' is {mime}; a status carries text or an image, and \
                                 nothing was sent."
                            )));
                        }
                        let thumbnail = match make_jpeg_thumbnail(&bytes) {
                            Some(t) => t,
                            None => {
                                return Ok(ToolResult::error(format!(
                                    "'{path}' could not be decoded as an image; nothing was sent."
                                )));
                            }
                        };
                        let upload = pget!(
                            upload_media(&client, bytes, wacore::download::MediaType::Image).await
                        );
                        client
                            .status()
                            .send_image(upload, thumbnail, text.as_deref(), &jids, opts)
                            .await
                            .map(|_| format!("image status from {path}"))
                    }
                    None => {
                        let body = text.as_deref().unwrap_or_default();
                        client
                            .status()
                            .send_text(
                                body,
                                broadcast::STATUS_BACKGROUND_ARGB,
                                waproto::whatsapp::message::extended_text_message::FontType::SYSTEM,
                                &jids,
                                opts,
                            )
                            .await
                            .map(|_| "text status".to_string())
                    }
                };
                match posted {
                    Ok(kind) => Ok(ToolResult::success(format!(
                        "Status update ({kind}) posted to {} allowlisted recipient(s), paced at \
                         {}s (at least {}s of wall clock).",
                        jids.len(),
                        pace.as_secs(),
                        broadcast::burst_floor(jids.len(), pace).as_secs()
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to post the status update: {e}"
                    ))),
                }
            }

            // ── list_newsletters ─────────────────────────────────────────────
            // #1485. Read-only, so no rails: it names what this account already
            // follows and is the only way to learn a newsletter JID to post to.
            "list_newsletters" => match client.newsletter().list_subscribed().await {
                Ok(list) if list.is_empty() => Ok(ToolResult::success(
                    "This WhatsApp account follows no newsletters (channels).".to_string(),
                )),
                Ok(list) => Ok(ToolResult::success(format!(
                    "{} newsletter(s):\n{}",
                    list.len(),
                    list.iter()
                        .map(|n| format!("- {} ({})", n.name, n.jid))
                        .collect::<Vec<_>>()
                        .join("\n")
                ))),
                Err(e) => Ok(ToolResult::error(format!(
                    "Failed to list newsletters: {e}"
                ))),
            },

            // ── newsletter (channel) reads ───────────────────────────────────
            // #1485. Both readers only report what the server already knows
            // about a channel, so neither posts anything and neither is gated.
            "newsletter_get_metadata" => {
                let jid = pget!(resolve_newsletter_jid(&input));
                match client.newsletter().get_metadata(&jid).await {
                    Ok(m) => Ok(ToolResult::success(describe_newsletter(&m))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to read {jid}: {e}"))),
                }
            }

            "newsletter_get_metadata_by_invite" => {
                let code = pget!(get_str(&input, "invite_code")).to_string();
                match client.newsletter().get_metadata_by_invite(&code).await {
                    Ok(m) => Ok(ToolResult::success(describe_newsletter(&m))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to read the channel behind invite '{code}': {e}"
                    ))),
                }
            }

            "newsletter_get_messages" => {
                let jid = pget!(resolve_newsletter_jid(&input));
                let count = input
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .clamp(1, 100) as u32;
                let before = input.get("before").and_then(|v| v.as_u64());
                match client
                    .newsletter()
                    .get_messages(jid.clone(), count, before)
                    .await
                {
                    Ok(list) if list.is_empty() => Ok(ToolResult::success(format!(
                        "{jid} returned no posts for that window."
                    ))),
                    Ok(list) => Ok(ToolResult::success(describe_newsletter_messages(
                        &jid, &list,
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to read {jid} history: {e}"
                    ))),
                }
            }

            // ── newsletter (channel) writes ──────────────────────────────────
            // #1485. Joining, leaving, creating, editing and revoking all move
            // a public surface: what this account follows, owns, or has posted
            // for every reader. They pass `send_scope` like any other outbound
            // act, and none is charged to the message budget because a channel
            // is not a conversation.
            "newsletter_join" => {
                let jid = pget!(resolve_newsletter_jid(&input));
                match client.newsletter().join(&jid).await {
                    Ok(m) => Ok(ToolResult::success(format!(
                        "Joined the channel.\n{}",
                        describe_newsletter(&m)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to join {jid}: {e}"))),
                }
            }

            "newsletter_leave" => {
                let jid = pget!(resolve_newsletter_jid(&input));
                match client.newsletter().leave(&jid).await {
                    Ok(()) => Ok(ToolResult::success(format!("Left {jid}."))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to leave {jid}: {e}"))),
                }
            }

            "newsletter_create" => {
                // The name travels in `message`, the same slot `create_label`
                // uses, so "the string that names this thing" has one spelling.
                let name = pget!(get_str(&input, "message")).to_string();
                let description = input.get("description").and_then(|v| v.as_str());
                match client.newsletter().create(&name, description).await {
                    Ok(m) => Ok(ToolResult::success(format!(
                        "Created the channel.\n{}",
                        describe_newsletter(&m)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to create the channel: {e}"
                    ))),
                }
            }

            "newsletter_update" => {
                let jid = pget!(resolve_newsletter_jid(&input));
                let name = input.get("message").and_then(|v| v.as_str());
                let description = input.get("description").and_then(|v| v.as_str());
                if name.is_none() && description.is_none() {
                    return Ok(ToolResult::error(
                        "newsletter_update needs 'message' (the new name) or 'description' \
                         (the new description) to have anything to change."
                            .to_string(),
                    ));
                }
                match client.newsletter().update(&jid, name, description).await {
                    Ok(m) => Ok(ToolResult::success(format!(
                        "Updated the channel.\n{}",
                        describe_newsletter(&m)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to update {jid}: {e}"))),
                }
            }

            "newsletter_revoke_message" => {
                let jid = pget!(resolve_newsletter_jid(&input));
                let msg_id = pget!(get_str(&input, "message_id")).to_string();
                match client
                    .newsletter()
                    .revoke_message(&jid, msg_id.clone())
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Deleted message {msg_id} from {jid}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to delete {msg_id} from {jid}: {e}"
                    ))),
                }
            }

            // ── community (parent groups) ────────────────────────────────────
            // #1485. A community is a group with `is_parent_group` set, so its
            // JID is an ordinary `@g.us` address and goes through the same
            // resolver as any other group. One spelling for "the group this
            // names" means a community can never be read as a chat, and the
            // resolver's server check applies to every action below.
            "community_create" => {
                let name = pget!(get_str(&input, "subject")).to_string();
                let mut options = CreateCommunityOptions::new(name);
                if let Some(desc) = input.get("description").and_then(|v| v.as_str()) {
                    options.description = Some(desc.to_string());
                }
                // The lib defaults these three, so an absent one keeps the lib
                // default instead of a second default invented here.
                if let Some(closed) = input.get("closed").and_then(|v| v.as_bool()) {
                    options.closed = closed;
                }
                if let Some(allow) = input
                    .get("allow_non_admin_sub_group_creation")
                    .and_then(|v| v.as_bool())
                {
                    options.allow_non_admin_sub_group_creation = allow;
                }
                if let Some(general) = input.get("create_general_chat").and_then(|v| v.as_bool()) {
                    options.create_general_chat = general;
                }
                match client.community().create(options).await {
                    Ok(created) => Ok(ToolResult::success(format!(
                        "Community '{}' created: {}.",
                        created.metadata.subject, created.metadata.id
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to create the community: {e}"
                    ))),
                }
            }

            "community_create_subgroup" => {
                let parent = pget!(resolve_group_jid(&input));
                let name = pget!(get_str(&input, "subject")).to_string();
                let participants = pget!(resolve_participants(&input));
                match client
                    .community()
                    .create_subgroup(name, &participants, parent.clone())
                    .await
                {
                    Ok(created) => Ok(ToolResult::success(format!(
                        "Subgroup '{}' created and linked to {parent}: {}.",
                        created.metadata.subject, created.metadata.id
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to create the subgroup: {e}"
                    ))),
                }
            }

            "community_deactivate" => {
                let jid = pget!(resolve_group_jid(&input));
                match client.community().deactivate(jid.clone()).await {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Community {jid} deactivated. Its subgroups were unlinked, not deleted."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to deactivate {jid}: {e}"
                    ))),
                }
            }

            "community_remove_participants" => {
                let jid = pget!(resolve_group_jid(&input));
                let participants = pget!(resolve_participants(&input));
                match client
                    .community()
                    .remove_participants(jid.clone(), &participants)
                    .await
                {
                    Ok(changes) => Ok(ToolResult::success(format!(
                        "Removed from {jid} and every linked group: {}",
                        summarize_participant_changes(&changes)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to remove participants: {e}"
                    ))),
                }
            }

            "community_link_subgroups" => {
                let jid = pget!(resolve_group_jid(&input));
                let subgroups = pget!(resolve_group_jids(&input));
                match client
                    .community()
                    .link_subgroups(jid.clone(), &subgroups)
                    .await
                {
                    Ok(result) => Ok(ToolResult::success(format!(
                        "Linked to {jid}: {} of {} group(s).{}",
                        result.linked_jids.len(),
                        subgroups.len(),
                        describe_failed_groups(&result.failed_groups)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to link subgroups to {jid}: {e}"
                    ))),
                }
            }

            "community_unlink_subgroups" => {
                let jid = pget!(resolve_group_jid(&input));
                let subgroups = pget!(resolve_group_jids(&input));
                let remove_orphan_members = input
                    .get("remove_orphan_members")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match client
                    .community()
                    .unlink_subgroups(jid.clone(), &subgroups, remove_orphan_members)
                    .await
                {
                    Ok(result) => Ok(ToolResult::success(format!(
                        "Unlinked from {jid}: {} of {} group(s).{}",
                        result.unlinked_jids.len(),
                        subgroups.len(),
                        describe_failed_groups(&result.failed_groups)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to unlink subgroups from {jid}: {e}"
                    ))),
                }
            }

            "community_join_subgroup" => {
                let jid = pget!(resolve_group_jid(&input));
                let subgroup = pget!(resolve_subgroup_jid(&input));
                match client
                    .community()
                    .join_subgroup(jid.clone(), subgroup.clone())
                    .await
                {
                    Ok(meta) => Ok(ToolResult::success(format!(
                        "Joined {} via {jid}.\n{}",
                        meta.id,
                        describe_community_group(&meta)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to join {subgroup} via {jid}: {e}"
                    ))),
                }
            }

            "community_query_linked_group" => {
                let jid = pget!(resolve_group_jid(&input));
                let subgroup = pget!(resolve_subgroup_jid(&input));
                match client
                    .community()
                    .query_linked_group(jid.clone(), subgroup.clone())
                    .await
                {
                    Ok(meta) => Ok(ToolResult::success(describe_community_group(&meta))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to read {subgroup} through {jid}: {e}"
                    ))),
                }
            }

            "community_get_subgroups" => {
                let jid = pget!(resolve_group_jid(&input));
                match client.community().get_subgroups(&jid).await {
                    Ok(list) if list.is_empty() => {
                        Ok(ToolResult::success(format!("{jid} reported no subgroups.")))
                    }
                    Ok(list) => Ok(ToolResult::success(describe_community_subgroups(
                        &jid, &list,
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to list the subgroups of {jid}: {e}"
                    ))),
                }
            }

            "community_get_subgroup_participant_counts" => {
                let jid = pget!(resolve_group_jid(&input));
                match client
                    .community()
                    .get_subgroup_participant_counts(&jid)
                    .await
                {
                    Ok(counts) if counts.is_empty() => Ok(ToolResult::success(format!(
                        "{jid} reported no subgroup counts."
                    ))),
                    Ok(counts) => {
                        let mut out = format!("{} subgroup(s) of {jid}:", counts.len());
                        for (sub, count) in &counts {
                            out.push_str(&format!("\n  {sub}: {count}"));
                        }
                        Ok(ToolResult::success(out))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to read the subgroup counts of {jid}: {e}"
                    ))),
                }
            }

            "community_get_linked_groups_participants" => {
                let jid = pget!(resolve_group_jid(&input));
                match client
                    .community()
                    .get_linked_groups_participants(jid.clone())
                    .await
                {
                    Ok(list) if list.is_empty() => Ok(ToolResult::success(format!(
                        "{jid} reported no participants across its linked groups."
                    ))),
                    Ok(list) => {
                        let mut out = format!(
                            "{} participant(s) across {jid}'s linked groups:",
                            list.len()
                        );
                        for p in &list {
                            out.push_str(&format!("\n  {}", p.jid));
                        }
                        Ok(ToolResult::success(out))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to read the participants of {jid}: {e}"
                    ))),
                }
            }

            "community_get_participating" => match client.community().get_participating().await {
                Ok(map) if map.is_empty() => Ok(ToolResult::success(
                    "This account is not in any community.".to_string(),
                )),
                Ok(map) => {
                    let mut entries: Vec<_> = map.into_iter().collect();
                    entries.sort_by_key(|(jid, _)| jid.to_string());
                    let mut out = format!("{} community(ies):", entries.len());
                    for (jid, meta) in entries {
                        out.push_str(&format!(
                            "\n  {jid} '{}' ({} participant(s))",
                            meta.subject,
                            meta.size.unwrap_or(meta.participants.len() as u32)
                        ));
                    }
                    Ok(ToolResult::success(out))
                }
                Err(e) => Ok(ToolResult::error(format!(
                    "Failed to list the communities this account is in: {e}"
                ))),
            },

            // ── create_label / assign_label ──────────────────────────────────
            // #1485. Labels are private organisation on the owner's own account:
            // nothing is delivered to anyone, so the broadcast rails do not
            // apply and the send budget is not charged.
            "create_label" => {
                let name = pget!(get_str(&input, "message")).to_string();
                let label_id = input
                    .get("label_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        // App state is an upsert keyed by label_id, so a caller
                        // that supplies none gets a fresh label rather than
                        // silently renaming an existing one.
                        uuid::Uuid::new_v4().to_string()
                    });
                let color = input
                    .get("label_color")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32;
                match client.labels().create_label(&label_id, &name, color).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Label '{name}' saved with id {label_id}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to save the label: {e}"))),
                }
            }

            "assign_label" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let label_id = pget!(get_str(&input, "label_id")).to_string();
                let remove = input
                    .get("remove")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let result = if remove {
                    client.labels().remove_chat_label(&label_id, &jid).await
                } else {
                    client.labels().add_chat_label(&label_id, &jid).await
                };
                match result {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Label {label_id} {} chat with {jid_str}.",
                        if remove { "removed from" } else { "added to" }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to update the label on {jid_str}: {e}"
                    ))),
                }
            }

            // ── block_contact ────────────────────────────────────────────────
            // #1487: the owner's first-line defence against a spamming number.
            // The resolved JID is logged so the block is auditable after the
            // fact, and the local mirror is updated so the inbound guard drops
            // that sender on the very next message without a server round trip.
            "block_contact" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                match client.blocking().block(&jid).await {
                    Ok(_) => {
                        tracing::info!(
                            target: "whatsapp",
                            jid = %jid,
                            "blocked contact on owner request (#1487)"
                        );
                        self.whatsapp_state.blocklist.insert(&jid.to_string()).await;
                        Ok(ToolResult::success(format!(
                            "Blocked {} on WhatsApp. Their messages no longer reach the bot.",
                            jid_str
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to block {jid_str}: {e}"))),
                }
            }

            // ── unblock_contact ──────────────────────────────────────────────
            "unblock_contact" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                match client.blocking().unblock(&jid).await {
                    Ok(_) => {
                        tracing::info!(
                            target: "whatsapp",
                            jid = %jid,
                            "unblocked contact on owner request (#1487)"
                        );
                        self.whatsapp_state.blocklist.remove(&jid.to_string()).await;
                        Ok(ToolResult::success(format!(
                            "Unblocked {} on WhatsApp.",
                            jid_str
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to unblock {jid_str}: {e}"
                    ))),
                }
            }

            // ── list_blocked ─────────────────────────────────────────────────
            // Reads the server, not the mirror: the owner may have blocked
            // someone from their phone, and reporting a stale local set as
            // fact would be worse than one extra round trip. The mirror is
            // refreshed from the answer.
            "list_blocked" => match client.blocking().get_blocklist().await {
                Ok(entries) => {
                    let jids: Vec<String> = entries.iter().map(|e| e.jid.to_string()).collect();
                    self.whatsapp_state.blocklist.replace(jids.clone()).await;
                    if jids.is_empty() {
                        return Ok(ToolResult::success(
                            "No blocked contacts on this WhatsApp account.".to_string(),
                        ));
                    }
                    Ok(ToolResult::success(format!(
                        "{} blocked contact(s):\n{}",
                        jids.len(),
                        jids.join("\n")
                    )))
                }
                Err(e) => Ok(ToolResult::error(format!(
                    "Failed to fetch the blocklist: {e}"
                ))),
            },

            // ── groups: lifecycle core ───────────────────────────────────────
            // Every group action goes through `client.groups()`. Admin actions
            // name their group explicitly (see `resolve_group_jid`): there is
            // no owner fallback, because guessing a target for a kick or a
            // promote is exactly the kind of mistake with no undo.
            "create_group" => {
                let subject = match GroupSubject::new(pget!(get_str(&input, "subject"))) {
                    Ok(s) => s,
                    Err(e) => return Ok(ToolResult::error(format!("Invalid subject: {e}"))),
                };
                let participants = pget!(resolve_participants(&input));
                let mut options = GroupCreateOptions::new(subject.into_string());
                options.participants = participants
                    .into_iter()
                    .map(GroupParticipantOptions::new)
                    .collect();
                if let Some(desc) = input.get("description").and_then(|v| v.as_str()) {
                    match GroupDescription::new(desc) {
                        Ok(d) => options.description = Some(d),
                        Err(e) => {
                            return Ok(ToolResult::error(format!("Invalid description: {e}")));
                        }
                    }
                }
                match client.groups().create_group(options).await {
                    Ok(created) => Ok(ToolResult::success(format!(
                        "Group '{}' created: {} ({} participant(s)).",
                        created.metadata.subject,
                        created.metadata.id,
                        created.metadata.participants.len()
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to create the group: {e}"
                    ))),
                }
            }

            "leave_group" => {
                let jid = pget!(resolve_group_jid(&input));
                match client.groups().leave(jid.clone()).await {
                    Ok(()) => Ok(ToolResult::success(format!("Left group {jid}."))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to leave {jid}: {e}"))),
                }
            }

            "set_group_subject" => {
                let jid = pget!(resolve_group_jid(&input));
                let subject = match GroupSubject::new(pget!(get_str(&input, "subject"))) {
                    Ok(s) => s,
                    Err(e) => return Ok(ToolResult::error(format!("Invalid subject: {e}"))),
                };
                match client.groups().set_subject(jid.clone(), subject).await {
                    Ok(()) => Ok(ToolResult::success(format!("Subject of {jid} updated."))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to set the subject: {e}"))),
                }
            }

            "set_group_description" => {
                let jid = pget!(resolve_group_jid(&input));
                // An empty string DELETES the description, which is a distinct
                // operation from leaving it alone. The parameter is therefore
                // required here: an absent one is an error, not a silent no-op.
                let raw = match input.get("description").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => {
                        return Ok(ToolResult::error(
                            "Missing required parameter 'description'. Pass an empty string to \
                             delete the group description."
                                .to_string(),
                        ));
                    }
                };
                let description = if raw.is_empty() {
                    None
                } else {
                    match GroupDescription::new(raw) {
                        Ok(d) => Some(d),
                        Err(e) => {
                            return Ok(ToolResult::error(format!("Invalid description: {e}")));
                        }
                    }
                };
                // `Resolve` costs one round trip but is the only variant that
                // is correct without holding fresh metadata.
                match client
                    .groups()
                    .set_description(jid.clone(), description, PreviousDescription::Resolve)
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Description of {jid} updated."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set the description: {e}"
                    ))),
                }
            }

            "add_participants" => {
                let jid = pget!(resolve_group_jid(&input));
                let participants = pget!(resolve_participants(&input));
                match client
                    .groups()
                    .add_participants(jid.clone(), &participants)
                    .await
                {
                    Ok(changes) => Ok(ToolResult::success(format!(
                        "Add to {jid}: {}",
                        summarize_participant_changes(&changes)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to add participants: {e}"
                    ))),
                }
            }

            "remove_participants" => {
                let jid = pget!(resolve_group_jid(&input));
                let participants = pget!(resolve_participants(&input));
                match client
                    .groups()
                    .remove_participants(jid.clone(), &participants)
                    .await
                {
                    Ok(changes) => Ok(ToolResult::success(format!(
                        "Remove from {jid}: {}",
                        summarize_participant_changes(&changes)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to remove participants: {e}"
                    ))),
                }
            }

            "promote_participants" => {
                let jid = pget!(resolve_group_jid(&input));
                let participants = pget!(resolve_participants(&input));
                match client
                    .groups()
                    .promote_participants(jid.clone(), &participants)
                    .await
                {
                    Ok(changes) => Ok(ToolResult::success(format!(
                        "Promote in {jid}: {}",
                        summarize_participant_changes(&changes)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to promote participants: {e}"
                    ))),
                }
            }

            "demote_participants" => {
                let jid = pget!(resolve_group_jid(&input));
                let participants = pget!(resolve_participants(&input));
                match client
                    .groups()
                    .demote_participants(jid.clone(), &participants)
                    .await
                {
                    Ok(changes) => Ok(ToolResult::success(format!(
                        "Demote in {jid}: {}",
                        summarize_participant_changes(&changes)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to demote participants: {e}"
                    ))),
                }
            }

            "get_group_metadata" => {
                let jid = pget!(resolve_group_jid(&input));
                match client.groups().get_metadata(&jid).await {
                    Ok(m) => {
                        let admins = m.participants.iter().filter(|p| p.is_admin()).count();
                        let mut out = format!(
                            "Group {} ('{}')\n  participants: {}\n  admins: {}\n  locked: {}\n  \
                             announcement-only: {}\n  membership approval: {}",
                            m.id,
                            m.subject,
                            m.size.unwrap_or(m.participants.len() as u32),
                            admins,
                            m.is_locked,
                            m.is_announcement,
                            m.membership_approval,
                        );
                        if let Some(desc) = m.description.as_deref().filter(|d| !d.is_empty()) {
                            out.push_str(&format!("\n  description: {desc}"));
                        }
                        Ok(ToolResult::success(out))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to read {jid}: {e}"))),
                }
            }

            "get_invite_link" => {
                let jid = pget!(resolve_group_jid(&input));
                let reset = input
                    .get("reset_invite")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match client.groups().get_invite_link(jid.clone(), reset).await {
                    Ok(link) => Ok(ToolResult::success(format!(
                        "Invite link for {jid}{}: {link}",
                        if reset {
                            " (the previous link was revoked)"
                        } else {
                            ""
                        }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to get the invite link: {e}"
                    ))),
                }
            }

            "join_with_invite_code" => {
                let code = pget!(get_str(&input, "invite_code")).to_string();
                match client.groups().join_with_invite_code(&code).await {
                    Ok(JoinGroupResult::Joined(jid)) => Ok(ToolResult::success(format!(
                        "Joined group {jid} via invite code."
                    ))),
                    Ok(JoinGroupResult::PendingApproval(jid)) => Ok(ToolResult::success(format!(
                        "Request to join {jid} sent; an admin must approve it before you are a \
                         member."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to join with that code: {e}"
                    ))),
                }
            }

            // ── groups: policy and admin ─────────────────────────────────────
            // The settings an admin toggles. Each arm takes the value it is
            // setting, required rather than defaulted: a group policy that flips
            // because a parameter was omitted is a silent change nobody asked
            // for, and there is no undo on the wire.
            "set_announce" => {
                let jid = pget!(resolve_group_jid(&input));
                let announce = pget!(get_bool(&input, "announce"));
                let described = if announce {
                    "announcement-only: only admins may send"
                } else {
                    "open: every member may send"
                };
                match client.groups().set_announce(jid.clone(), announce).await {
                    Ok(()) => Ok(ToolResult::success(format!("{jid} is now {described}."))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set announcement mode: {e}"
                    ))),
                }
            }

            "set_locked" => {
                let jid = pget!(resolve_group_jid(&input));
                let locked = pget!(get_bool(&input, "locked"));
                let described = if locked {
                    "locked: only admins may edit group info"
                } else {
                    "unlocked: every member may edit group info"
                };
                match client.groups().set_locked(jid.clone(), locked).await {
                    Ok(()) => Ok(ToolResult::success(format!("{jid} is now {described}."))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to set locked: {e}"))),
                }
            }

            "set_ephemeral" => {
                let jid = pget!(resolve_group_jid(&input));
                let expiration = match input.get("expiration").and_then(|v| v.as_u64()) {
                    Some(v) if v <= u32::MAX as u64 => v as u32,
                    Some(v) => {
                        return Ok(ToolResult::error(format!(
                            "'expiration' value {v} is out of range; the maximum is {}.",
                            u32::MAX
                        )));
                    }
                    None => {
                        return Ok(ToolResult::error(
                            "Missing required parameter 'expiration' (seconds; 0 disables \
                             disappearing messages)."
                                .to_string(),
                        ));
                    }
                };
                let described = if expiration == 0 {
                    "disabled".to_string()
                } else {
                    format!("{expiration}s")
                };
                match client.groups().set_ephemeral(jid.clone(), expiration).await {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Disappearing messages for {jid}: {described}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set the disappearing-message timer: {e}"
                    ))),
                }
            }

            "set_membership_approval" => {
                let jid = pget!(resolve_group_jid(&input));
                let mode = match pget!(get_str(&input, "membership_approval")) {
                    "off" => MembershipApprovalMode::Off,
                    "on" => MembershipApprovalMode::On,
                    other => {
                        return Ok(ToolResult::error(format!(
                            "Invalid 'membership_approval' value '{other}'. Expected 'off' or \
                             'on'."
                        )));
                    }
                };
                let described = if matches!(mode, MembershipApprovalMode::On) {
                    "required: an admin must approve each join request"
                } else {
                    "not required: anyone with the link may join"
                };
                match client
                    .groups()
                    .set_membership_approval(jid.clone(), mode)
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!("{jid}: {described}."))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set membership approval: {e}"
                    ))),
                }
            }

            "set_member_add_mode" => {
                let jid = pget!(resolve_group_jid(&input));
                let mode = match pget!(get_str(&input, "member_add_mode")) {
                    "admin_add" => MemberAddMode::AdminAdd,
                    "all_member_add" => MemberAddMode::AllMemberAdd,
                    other => {
                        return Ok(ToolResult::error(format!(
                            "Invalid 'member_add_mode' value '{other}'. Expected 'admin_add' or \
                             'all_member_add'."
                        )));
                    }
                };
                let described = if matches!(mode, MemberAddMode::AllMemberAdd) {
                    "any member may add"
                } else {
                    "admins only"
                };
                match client.groups().set_member_add_mode(jid.clone(), mode).await {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Who may add members to {jid}: {described}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set the member-add mode: {e}"
                    ))),
                }
            }

            "set_limit_sharing" => {
                let jid = pget!(resolve_group_jid(&input));
                let enabled = pget!(get_bool(&input, "limit_sharing"));
                match client.groups().set_limit_sharing(&jid, enabled).await {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Sharing restrictions for {jid}: {}.",
                        if enabled { "enabled" } else { "disabled" }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set limit-sharing: {e}"
                    ))),
                }
            }

            "set_group_history" => {
                let jid = pget!(resolve_group_jid(&input));
                let enabled = pget!(get_bool(&input, "group_history"));
                match client
                    .groups()
                    .set_group_history(jid.clone(), enabled)
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "History sharing for {jid}: {}.",
                        if enabled { "enabled" } else { "disabled" }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set group history: {e}"
                    ))),
                }
            }

            "set_no_frequently_forwarded" => {
                let jid = pget!(resolve_group_jid(&input));
                let restrict = pget!(get_bool(&input, "no_frequently_forwarded"));
                match client
                    .groups()
                    .set_no_frequently_forwarded(jid.clone(), restrict)
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Forwarding of frequently-forwarded messages in {jid}: {}.",
                        if restrict { "blocked" } else { "allowed" }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set the frequently-forwarded restriction: {e}"
                    ))),
                }
            }

            "set_allow_admin_reports" => {
                let jid = pget!(resolve_group_jid(&input));
                let allow = pget!(get_bool(&input, "allow_admin_reports"));
                match client
                    .groups()
                    .set_allow_admin_reports(jid.clone(), allow)
                    .await
                {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "Member reports to admins in {jid}: {}.",
                        if allow { "allowed" } else { "blocked" }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set admin reports: {e}"
                    ))),
                }
            }

            "get_membership_requests" => {
                let jid = pget!(resolve_group_jid(&input));
                match client.groups().get_membership_requests(jid.clone()).await {
                    Ok(requests) if requests.is_empty() => Ok(ToolResult::success(format!(
                        "{jid}: no pending membership requests."
                    ))),
                    Ok(requests) => {
                        let mut out = format!("{jid}: {} pending request(s).", requests.len());
                        for r in &requests {
                            out.push_str(&format!("\n  {}", r.jid));
                            if let Some(t) = r.request_time {
                                out.push_str(&format!(" (requested at {t})"));
                            }
                        }
                        Ok(ToolResult::success(out))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to list membership requests: {e}"
                    ))),
                }
            }

            "approve_membership_requests" => {
                let jid = pget!(resolve_group_jid(&input));
                let participants = pget!(resolve_participants(&input));
                match client
                    .groups()
                    .approve_membership_requests(jid.clone(), &participants)
                    .await
                {
                    Ok(changes) => Ok(ToolResult::success(format!(
                        "Approved membership requests for {jid}: {}",
                        summarize_participant_changes(&changes)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to approve membership requests: {e}"
                    ))),
                }
            }

            "reject_membership_requests" => {
                let jid = pget!(resolve_group_jid(&input));
                let participants = pget!(resolve_participants(&input));
                match client
                    .groups()
                    .reject_membership_requests(jid.clone(), &participants)
                    .await
                {
                    Ok(changes) => Ok(ToolResult::success(format!(
                        "Rejected membership requests for {jid}: {}",
                        summarize_participant_changes(&changes)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to reject membership requests: {e}"
                    ))),
                }
            }

            "cancel_membership_requests" => {
                let jid = pget!(resolve_group_jid(&input));
                let participants = pget!(resolve_participants(&input));
                match client
                    .groups()
                    .cancel_membership_requests(jid.clone(), &participants)
                    .await
                {
                    Ok(changes) => Ok(ToolResult::success(format!(
                        "Cancelled membership requests for {jid}: {}",
                        summarize_participant_changes(&changes)
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to cancel membership requests: {e}"
                    ))),
                }
            }

            "get_participating" => match client.groups().get_participating().await {
                Ok(groups) if groups.is_empty() => Ok(ToolResult::success(
                    "Not participating in any group.".to_string(),
                )),
                Ok(groups) => {
                    let mut entries: Vec<_> = groups.into_iter().collect();
                    entries.sort_by(|a, b| a.1.subject.cmp(&b.1.subject));
                    let mut out = format!("Participating in {} group(s):", entries.len());
                    for (jid, meta) in entries {
                        let admins = meta.participants.iter().filter(|p| p.is_admin()).count();
                        out.push_str(&format!(
                            "\n  {} ('{}'): {} participants, {} admin(s)",
                            jid,
                            meta.subject,
                            meta.participants.len(),
                            admins
                        ));
                    }
                    Ok(ToolResult::success(out))
                }
                Err(e) => Ok(ToolResult::error(format!(
                    "Failed to list participating groups: {e}"
                ))),
            },

            "is_admin" => {
                let jid = pget!(resolve_group_jid(&input));
                let wanted = match input.get("participants").and_then(|v| v.as_array()) {
                    Some(list) if !list.is_empty() => Some(pget!(resolve_participants(&input))),
                    _ => None,
                };
                match client.groups().get_metadata(&jid).await {
                    Ok(m) => {
                        let mut out = format!("Admins in {jid} ('{}'):", m.subject);
                        let mut listed = 0usize;
                        for p in &m.participants {
                            if !p.is_admin() {
                                continue;
                            }
                            let is_wanted = match &wanted {
                                Some(list) => list.iter().any(|w| {
                                    *w == p.jid
                                        || p.phone_number.as_ref() == Some(w)
                                        || p.lid.as_ref() == Some(w)
                                }),
                                None => true,
                            };
                            if !is_wanted {
                                continue;
                            }
                            out.push_str(&format!(
                                "\n  {}{}",
                                p.jid,
                                if p.is_super_admin() {
                                    " (super admin)"
                                } else {
                                    ""
                                }
                            ));
                            listed += 1;
                        }
                        if listed == 0 {
                            out = match &wanted {
                                Some(list) => {
                                    let names: Vec<String> =
                                        list.iter().map(|j| j.to_string()).collect();
                                    format!(
                                        "None of {} is an admin in {jid} ('{}').",
                                        names.join(", "),
                                        m.subject
                                    )
                                }
                                None => {
                                    format!("{jid} ('{}') lists no admin participants.", m.subject)
                                }
                            };
                        }
                        Ok(ToolResult::success(out))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to check admin status in {jid}: {e}"
                    ))),
                }
            }
            "get_profile_picture" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let preview = match input.get("preview") {
                    Some(_) => pget!(get_bool(&input, "preview")),
                    None => false,
                };
                match client.contacts().get_profile_picture(&jid, preview).await {
                    Ok(Some(pic)) => Ok(ToolResult::success(format!(
                        "{jid_str}: {} profile picture at {} (id {}).",
                        if preview { "preview" } else { "full-size" },
                        pic.url,
                        pic.id
                    ))),
                    Ok(None) => Ok(ToolResult::success(format!(
                        "{jid_str}: no profile picture is visible to this account."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to fetch the profile picture for {jid_str}: {e}"
                    ))),
                }
            }

            "get_group_profile_picture" => {
                let jids = pget!(resolve_group_jids(&input));
                let preview = match input.get("preview") {
                    Some(_) => pget!(get_bool(&input, "preview")),
                    None => false,
                };
                let picture_type = if preview {
                    PictureType::Preview
                } else {
                    PictureType::Image
                };
                match client
                    .groups()
                    .get_profile_pictures(jids, picture_type)
                    .await
                {
                    Ok(pics) if pics.is_empty() => Ok(ToolResult::success(
                        "No group profile pictures returned.".to_string(),
                    )),
                    Ok(pics) => {
                        let mut out = format!("{} group picture(s):", pics.len());
                        for p in &pics {
                            out.push_str(&format!(
                                "\n  {}: {}",
                                p.group_jid,
                                p.url.as_deref().unwrap_or("no picture")
                            ));
                        }
                        Ok(ToolResult::success(out))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to fetch group profile pictures: {e}"
                    ))),
                }
            }

            "is_on_whatsapp" => {
                let jids = pget!(resolve_contacts(&input));
                match client.contacts().is_on_whatsapp(&jids).await {
                    Ok(results) if results.is_empty() => Ok(ToolResult::success(
                        "No lookup results returned.".to_string(),
                    )),
                    Ok(results) => {
                        let mut lines: Vec<String> = Vec::with_capacity(results.len());
                        for r in &results {
                            let mut line = if r.is_registered {
                                format!("{}: on WhatsApp", r.jid)
                            } else {
                                format!("{}: NOT registered on WhatsApp", r.jid)
                            };
                            if let Some(name) =
                                r.verified_name.as_ref().and_then(|v| v.name.as_ref())
                            {
                                line.push_str(&format!(" (business: {name})"));
                            }
                            if let Some(code) = r.contact_error.as_ref().and_then(|e| e.code) {
                                line.push_str(&format!(" [lookup error {code}]"));
                            }
                            lines.push(line);
                        }
                        Ok(ToolResult::success(lines.join("\n")))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to check whether the numbers are on WhatsApp: {e}"
                    ))),
                }
            }

            "get_user_info" => {
                let jids = pget!(resolve_contacts(&input));
                match client.contacts().get_user_info(&jids).await {
                    Ok(info) if info.is_empty() => {
                        Ok(ToolResult::success("No user info returned.".to_string()))
                    }
                    Ok(info) => {
                        let mut entries: Vec<_> = info.into_iter().collect();
                        entries.sort_by_key(|(jid, _)| jid.to_string());
                        let mut out = format!("{} contact(s):", entries.len());
                        for (jid, u) in entries {
                            let name = u
                                .verified_name
                                .as_ref()
                                .and_then(|v| v.name.clone())
                                .unwrap_or_else(|| "none".to_string());
                            out.push_str(&format!(
                                "\n  {}: verified name '{name}', status '{}', {} device(s)",
                                jid,
                                u.status.as_deref().unwrap_or("none"),
                                u.devices.len()
                            ));
                        }
                        Ok(ToolResult::success(out))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to fetch user info: {e}"))),
                }
            }

            "archive_chat" | "unarchive_chat" => {
                let (jid, jid_str) = pget!(
                    resolve_chat_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let archiving = action == "archive_chat";
                let result = if archiving {
                    client.chat_actions().archive_chat(&jid, None).await
                } else {
                    client.chat_actions().unarchive_chat(&jid, None).await
                };
                match result {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Chat with {} {}.",
                        jid_str,
                        if archiving { "archived" } else { "unarchived" }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to {} chat {jid_str}: {e}",
                        if archiving { "archive" } else { "unarchive" }
                    ))),
                }
            }

            "mute_chat" | "unmute_chat" => {
                let (jid, jid_str) = pget!(
                    resolve_chat_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let muting = action == "mute_chat";
                let mute_until = match input.get("mute_until_ms") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(pget!(v.as_i64().ok_or_else(|| {
                        ToolResult::error(
                            "'mute_until_ms' must be an integer Unix epoch timestamp in \
                         milliseconds."
                                .to_string(),
                        )
                    }))),
                };
                let result = if !muting {
                    client.chat_actions().unmute_chat(&jid).await
                } else {
                    match mute_until {
                        Some(until) => client.chat_actions().mute_chat_until(&jid, until).await,
                        None => client.chat_actions().mute_chat(&jid).await,
                    }
                };
                match result {
                    Ok(_) => Ok(ToolResult::success(match (muting, mute_until) {
                        (false, _) => format!("Chat with {jid_str} unmuted."),
                        (true, Some(until)) => {
                            format!("Chat with {jid_str} muted until {until} (epoch ms).")
                        }
                        (true, None) => format!("Chat with {jid_str} muted indefinitely."),
                    })),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to {} chat {jid_str}: {e}",
                        if muting { "mute" } else { "unmute" }
                    ))),
                }
            }

            "star_message" | "unstar_message" => {
                let (jid, jid_str) = pget!(
                    resolve_chat_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let msg_id = pget!(get_str(&input, "message_id")).to_string();
                let from_me = input
                    .get("from_me")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let participant = pget!(resolve_optional_participant(&input));
                let starring = action == "star_message";
                let result = if starring {
                    client
                        .chat_actions()
                        .star_message(&jid, participant.as_ref(), &msg_id, from_me)
                        .await
                } else {
                    client
                        .chat_actions()
                        .unstar_message(&jid, participant.as_ref(), &msg_id, from_me)
                        .await
                };
                match result {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Message {msg_id} in {jid_str} {}.",
                        if starring { "starred" } else { "unstarred" }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to {} message {msg_id} in {jid_str}: {e}",
                        if starring { "star" } else { "unstar" }
                    ))),
                }
            }

            "clear_chat" => {
                let (jid, jid_str) = pget!(
                    resolve_chat_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let delete_starred = match input.get("delete_starred") {
                    Some(_) => pget!(get_bool(&input, "delete_starred")),
                    None => false,
                };
                let delete_media = match input.get("delete_media") {
                    Some(_) => pget!(get_bool(&input, "delete_media")),
                    None => false,
                };
                let result = client
                    .chat_actions()
                    .clear_chat(&jid, delete_starred, delete_media, None)
                    .await;
                let mut note = String::new();
                if delete_starred {
                    note.push_str(", starred messages included");
                }
                if delete_media {
                    note.push_str(", media included");
                }
                match result {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Chat with {jid_str} cleared{note}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to clear chat {jid_str}: {e}"
                    ))),
                }
            }

            "save_contact" => {
                // A contact is saved by number, so there is no chat or owner to
                // fall back to: without 'phone' this would save the wrong person.
                if input.get("phone").and_then(|v| v.as_str()).is_none() {
                    return Ok(ToolResult::error(
                        "save_contact requires 'phone': a contact is saved by number, so there \
                         is no chat or owner to fall back to."
                            .to_string(),
                    ));
                }
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let full_name = input
                    .get("full_name")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let first_name = input
                    .get("first_name")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                if full_name.is_none() && first_name.is_none() {
                    return Ok(ToolResult::error(
                        "save_contact needs 'full_name' or 'first_name' (at least one)."
                            .to_string(),
                    ));
                }
                let save_on_primary = match input.get("save_on_primary_addressbook") {
                    Some(_) => pget!(get_bool(&input, "save_on_primary_addressbook")),
                    None => true,
                };
                let shown = full_name.clone().or_else(|| first_name.clone());
                let result = client
                    .chat_actions()
                    .save_contact(&jid, full_name, first_name, save_on_primary)
                    .await;
                match result {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Contact {jid_str} saved as '{}'{}.",
                        shown.as_deref().unwrap_or(""),
                        if save_on_primary {
                            " (also to the phone's address book)"
                        } else {
                            ""
                        }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to save contact {jid_str}: {e}"
                    ))),
                }
            }

            // ── presence: our own online state ───────────────────────────────
            // Presence is not a message, so these are not charged to the send
            // budget. The channel sets `available` on connect, so exposing both
            // directions here is what makes going offline reversible: an agent
            // that could only go unavailable would have to reconnect to return.
            "set_available" | "set_unavailable" => {
                let going_online = action == "set_available";
                let result = if going_online {
                    client.presence().set_available().await
                } else {
                    client.presence().set_unavailable().await
                };
                let state = if going_online {
                    "available"
                } else {
                    "unavailable"
                };
                match result {
                    Ok(()) => Ok(ToolResult::success(format!("Presence set to {state}."))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set presence to {state}: {e}"
                    ))),
                }
            }

            // ── presence: watching a contact ─────────────────────────────────
            // A subscription names the contact whose updates we want, so it
            // requires an explicit `phone`: the session-then-owner fallback
            // would subscribe to whoever happens to be talking, which is not
            // what the caller asked for.
            "subscribe_presence" | "unsubscribe_presence" => {
                if input.get("phone").and_then(|v| v.as_str()).is_none() {
                    return Ok(ToolResult::error(
                        "subscribe_presence and unsubscribe_presence need 'phone': the \
                         subscription names the contact to watch, and there is no sensible \
                         fallback."
                            .to_string(),
                    ));
                }
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let subscribing = action == "subscribe_presence";
                let result = if subscribing {
                    client.presence().subscribe(jid.clone()).await
                } else {
                    client.presence().unsubscribe(&jid).await
                };
                match result {
                    Ok(()) => Ok(ToolResult::success(format!(
                        "{} presence updates from {jid_str}.",
                        if subscribing {
                            "Subscribed to"
                        } else {
                            "Unsubscribed from"
                        }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to {} presence updates from {jid_str}: {e}",
                        if subscribing {
                            "subscribe to"
                        } else {
                            "unsubscribe from"
                        }
                    ))),
                }
            }

            // ── create_quiz ──────────────────────────────────────────────────
            // A quiz is a single-select poll that carries a correct answer, so
            // it shares `poll_options` with send_poll and adds only the index.
            // The lib mints the message secret for both, which is what makes a
            // quiz's votes readable through the same path a poll's are.
            "create_quiz" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the quiz");
                let question = pget!(get_str(&input, "poll_question")).to_string();
                let opts = pget!(poll_options(&input));
                // Not defaulted: option 0 is a real answer, so "absent" and
                // "the first option" must not be allowed to look alike.
                let Some(correct_index) = input
                    .get("quiz_correct_index")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                else {
                    return Ok(ToolResult::error(
                        "create_quiz needs 'quiz_correct_index': the 0-based index of the \
                         correct option in 'poll_options'."
                            .to_string(),
                    ));
                };
                if correct_index >= opts.len() {
                    return Ok(ToolResult::error(format!(
                        "'quiz_correct_index' {correct_index} is outside 'poll_options' (0..{}).",
                        opts.len()
                    )));
                }
                match client
                    .polls()
                    .create_quiz(jid, &question, &opts, correct_index)
                    .await
                {
                    Ok((result, _secret)) => {
                        self.whatsapp_state
                            .polls
                            .remember(result.message_id, opts)
                            .await;
                        Ok(ToolResult::success(format!(
                            "Quiz '{}' sent to {} via WhatsApp.",
                            question, jid_str
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to send quiz: {}", e))),
                }
            }

            // ── vote ─────────────────────────────────────────────────────────
            // Casting our own vote. The poll's message secret keys the vote
            // encryption and is already persisted for every poll this account
            // authored, so it is read back rather than asked for; `poll_secret`
            // exists for a poll authored somewhere else.
            "vote" => {
                let (jid, jid_str) = pget!(
                    resolve_chat_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the vote");
                let poll_id = pget!(get_str(&input, "poll_msg_id")).to_string();
                let selected = pget!(string_list(&input, "option_names"));
                let creator = pget!(poll_creator(&client, &input));
                let chat = jid.to_non_ad_string();
                let creator_str = creator.to_non_ad_string();
                let secret = pget!(
                    resolve_poll_secret(&client, &input, &chat, &creator_str, &poll_id).await
                );
                match client
                    .polls()
                    .vote(jid, &poll_id, &creator, &secret, &selected)
                    .await
                {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Voted on poll {poll_id} in {jid_str}: {}.",
                        selected.join(", ")
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to vote: {}", e))),
                }
            }

            // ── decrypt_vote ─────────────────────────────────────────────────
            // The escape hatch for a vote the inbound decoder could not label:
            // it takes the raw ciphertext and reports what it opens to. With
            // the poll's own option list still inside the window the answer is
            // labels; without it the option hashes are reported, which is the
            // honest answer rather than a guess at what they meant.
            "decrypt_vote" => {
                let (jid, jid_str) = pget!(
                    resolve_chat_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let poll_id = pget!(get_str(&input, "poll_msg_id")).to_string();
                let creator = pget!(poll_creator(&client, &input));
                let voter = pget!(parse_contact_jid(
                    pget!(get_str(&input, "voter_jid")),
                    "voter_jid"
                ));
                let payload = pget!(decode_b64(&input, "vote_payload"));
                let iv = pget!(decode_b64(&input, "vote_iv"));
                let chat = jid.to_non_ad_string();
                let creator_str = creator.to_non_ad_string();
                let secret = pget!(
                    resolve_poll_secret(&client, &input, &chat, &creator_str, &poll_id).await
                );
                let ciphertext = PollVoteCiphertext {
                    enc_payload: &payload,
                    enc_iv: &iv,
                };
                match client
                    .polls()
                    .decrypt_vote(ciphertext, &secret, &poll_id, &creator, &voter)
                    .await
                {
                    Ok(hashes) => {
                        let opened = match self.whatsapp_state.polls.get(&poll_id).await {
                            Some(options) => resolve_options(&hashes, &options).join(", "),
                            None => hashes
                                .iter()
                                .map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>())
                                .collect::<Vec<_>>()
                                .join(", "),
                        };
                        if opened.is_empty() {
                            return Ok(ToolResult::success(format!(
                                "The vote from {voter} on poll {poll_id} opens to no option: the \
                                 voter cleared their answer, or the ciphertext belongs to \
                                 another poll."
                            )));
                        }
                        Ok(ToolResult::success(format!(
                            "Vote from {voter} on poll {poll_id} in {jid_str}: {opened}"
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to decrypt the vote: {}",
                        e
                    ))),
                }
            }

            // ── aggregate_votes ──────────────────────────────────────────────
            // "How many picked what" - the question that makes a poll worth
            // running. The counting happens in the inbound decoder, which
            // already resolves each arriving vote to labels; this only reads
            // that tally back. It deliberately does NOT go through the lib's
            // crypto aggregate, which needs every voter's raw ciphertext, and
            // those are not retained: the decoded labels are.
            "aggregate_votes" => {
                let poll_id = pget!(get_str(&input, "poll_msg_id")).to_string();
                let tally = self.whatsapp_state.votes.for_poll(&poll_id).await;
                if tally.is_empty() {
                    return Ok(ToolResult::success(format!(
                        "No votes recorded for poll {poll_id}. Votes are counted as they arrive \
                         on the live connection, so a poll outside the tracked window reads as \
                         empty rather than as zero."
                    )));
                }
                // The poll's own option list keeps the report in the order the
                // poll asked its question; `poll_options` is the fallback for a
                // list that has already aged out of the window.
                let options = match self.whatsapp_state.polls.get(&poll_id).await {
                    Some(options) => Some(options),
                    None => match input.get("poll_options").and_then(|v| v.as_array()) {
                        Some(_) => Some(pget!(poll_options(&input))),
                        None => None,
                    },
                };
                let mut order: Vec<String> = options.clone().unwrap_or_default();
                for (_, chosen) in &tally {
                    for label in chosen {
                        if !order.iter().any(|known| known == label) {
                            order.push(label.clone());
                        }
                    }
                }
                let mut lines = vec![format!(
                    "Poll {poll_id}: {} vote(s) from {} voter(s).",
                    tally.len(),
                    tally.len()
                )];
                for label in &order {
                    let voters: Vec<&str> = tally
                        .iter()
                        .filter(|(_, chosen)| chosen.iter().any(|c| c == label))
                        .map(|(who, _)| who.as_str())
                        .collect();
                    lines.push(format!("- \"{label}\": {}", voters.len()));
                    if !voters.is_empty() {
                        lines.push(format!("  {}", voters.join(", ")));
                    }
                }
                Ok(ToolResult::success(lines.join("\n")))
            }

            // ── create_event ─────────────────────────────────────────────────
            // #1485. An event carries a per-message secret, exactly like a
            // poll: responders derive their RSVP key from it, so the creator
            // has to keep it or the answers can never be read back. It is
            // reported rather than swallowed for that reason.
            "create_event" => {
                let jid = pget!(resolve_group_jid(&input));
                let name = pget!(get_str(&input, "message")).to_string();
                let params = EventCreationParams {
                    name: name.clone(),
                    description: input
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    start_time: input.get("start_time").and_then(|v| v.as_i64()),
                    end_time: input.get("end_time").and_then(|v| v.as_i64()),
                    join_link: input
                        .get("join_link")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    is_scheduled_call: input.get("is_scheduled_call").and_then(|v| v.as_bool()),
                    extra_guests_allowed: input
                        .get("extra_guests_allowed")
                        .and_then(|v| v.as_bool()),
                    ..Default::default()
                };
                let jid_str = jid.to_string();
                gate_send!(self, jid_str, "the event");
                match client.events().create(jid.clone(), params).await {
                    Ok((result, secret)) => {
                        use base64::Engine as _;
                        let encoded = base64::engine::general_purpose::STANDARD.encode(&secret);
                        Ok(ToolResult::success(format!(
                            "Created event '{name}' in {jid_str}.\nEvent message id: {}\n\
                             Event secret (base64, needed to decrypt the RSVPs): {encoded}",
                            result.message_id
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to create the event: {e}"
                    ))),
                }
            }

            // ── respond_event ────────────────────────────────────────────────
            // #1485. The RSVP is encrypted with the event's own secret, so it
            // cannot be sent without one: 'event_secret' is the base64 string
            // 'create_event' returned when the event was made.
            "respond_event" => {
                let jid = pget!(resolve_group_jid(&input));
                let event_msg_id = pget!(get_str(&input, "event_msg_id")).to_string();
                let creator_jid: Jid = match pget!(get_str(&input, "event_creator_jid")).parse() {
                    Ok(j) => j,
                    Err(e) => {
                        return Ok(ToolResult::error(format!(
                            "'event_creator_jid' is not a JID: {e}"
                        )));
                    }
                };
                let secret = pget!(decode_b64(&input, "event_secret"));
                let response = match pget!(get_str(&input, "event_response")) {
                    "going" => EventResponseType::GOING,
                    "not_going" => EventResponseType::NOT_GOING,
                    "maybe" => EventResponseType::MAYBE,
                    other => {
                        return Ok(ToolResult::error(format!(
                            "Invalid 'event_response' value '{other}'. Expected 'going', \
                             'not_going', or 'maybe'."
                        )));
                    }
                };
                let guests = input
                    .get("extra_guest_count")
                    .and_then(|v| v.as_i64())
                    .map(|n| n as i32);
                let jid_str = jid.to_string();
                gate_send!(self, jid_str, "the RSVP");
                match client
                    .events()
                    .respond(
                        jid.clone(),
                        &event_msg_id,
                        &creator_jid,
                        &secret,
                        response,
                        guests,
                    )
                    .await
                {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "RSVP sent to {jid_str} for event {event_msg_id}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send the RSVP: {e}"))),
                }
            }

            // ── post_comment ─────────────────────────────────────────────────
            // #1485. A channel comment is encrypted with the parent post's
            // secret under the 'Enc Comment' use-case, so it needs the parent
            // key: the post id, and the post author so receivers can key the
            // decryption.
            "post_comment" => {
                let jid = pget!(resolve_group_jid(&input));
                let parent_id = pget!(get_str(&input, "parent_msg_id")).to_string();
                let author = input.get("parent_participant").and_then(|v| v.as_str());
                let text = pget!(get_str(&input, "message")).to_string();
                let jid_str = jid.to_string();
                let parent_key = waproto::whatsapp::MessageKey {
                    remote_jid: Some(jid_str.clone()),
                    from_me: Some(false),
                    id: Some(parent_id.clone()),
                    participant: author.map(str::to_string),
                };
                gate_send!(self, jid_str, "the comment");
                match client
                    .comments()
                    .send_text(jid.clone(), parent_key, &text)
                    .await
                {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Comment posted to {jid_str} under post {parent_id}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to post the comment: {e}"
                    ))),
                }
            }

            // ── wa_introspect ────────────────────────────────────────────────
            // #1485. The client's counters, node waiters and pacing knobs,
            // reachable against the live session so a running bot can be
            // diagnosed and retuned without a rebuild.
            "wa_introspect" => match pget!(get_str(&input, "debug_op")) {
                "stats" => match self.whatsapp_state.client_stats().await {
                    Some(s) => Ok(ToolResult::success(format!(
                        "WhatsApp session counters:\n- wire bytes: {} sent, {} received\n\
                         - frames: {} sent, {} received\n- messages: {} sent, {} received\n\
                         - events dropped: {}\n- reconnects: {} ({} consecutive errors)\n\
                         - resends throttled by the per-chat bucket: {}\n\
                         - last data received at {} ms since the epoch",
                        s.bytes_sent,
                        s.bytes_received,
                        s.frames_sent,
                        s.frames_received,
                        s.messages_sent,
                        s.messages_received,
                        s.events_dropped,
                        s.reconnects,
                        s.reconnect_errors,
                        s.resends_throttled,
                        s.last_data_received_ms
                    ))),
                    None => Ok(ToolResult::error(NOT_CONNECTED.to_string())),
                },
                op @ ("wait_for_node" | "wait_for_sent_node") => {
                    let tag = pget!(get_str(&input, "node_tag"));
                    let key = input.get("node_attr_key").and_then(|v| v.as_str());
                    let value = input.get("node_attr_value").and_then(|v| v.as_str());
                    // An attribute narrows a filter, so half of one is a
                    // request the caller did not make: refuse rather than
                    // silently match on the tag alone and report a node they
                    // were not asking about.
                    let attr = match (key, value) {
                        (Some(k), Some(v)) => Some((k, v)),
                        (None, None) => None,
                        _ => {
                            return Ok(ToolResult::error(
                                "'node_attr_key' and 'node_attr_value' are set together or \
                                 not at all."
                                    .to_string(),
                            ));
                        }
                    };
                    let timeout = std::time::Duration::from_millis(
                        input
                            .get("timeout_ms")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(WAIT_TIMEOUT.as_millis() as u64)
                            .clamp(100, 60_000),
                    );
                    let waited = if op == "wait_for_node" {
                        self.whatsapp_state.wait_for_node(tag, attr, timeout).await
                    } else {
                        self.whatsapp_state
                            .wait_for_sent_node(tag, attr, timeout)
                            .await
                    };
                    match waited {
                        Some(node) => Ok(ToolResult::success(format!(
                            "Node matching tag '{tag}':\n{node}"
                        ))),
                        None => Ok(ToolResult::error(NOT_CONNECTED.to_string())),
                    }
                }
                "acquire_raw_node" => match self.whatsapp_state.acquire_raw_node_forwarding().await
                {
                    Some(true) => Ok(ToolResult::success(
                        "Raw stanza forwarding was already on; the existing lease is kept."
                            .to_string(),
                    )),
                    Some(false) => Ok(ToolResult::success(
                        "Raw stanza forwarding is on, so Event::RawNode now fires for this \
                         session. 'release_raw_node' turns it off."
                            .to_string(),
                    )),
                    None => Ok(ToolResult::error(NOT_CONNECTED.to_string())),
                },
                "release_raw_node" => Ok(ToolResult::success(
                    if self.whatsapp_state.release_raw_node_forwarding() {
                        "Raw-stanza lease released. Forwarding stops unless another lease is \
                         still held."
                            .to_string()
                    } else {
                        "No raw-stanza lease was held, so nothing changed.".to_string()
                    },
                )),
                "set_resend_rate_limit" => {
                    let burst = u32_param(&input, "burst");
                    let refill = u32_param(&input, "refill_per_min");
                    if self
                        .whatsapp_state
                        .set_resend_rate_limit(burst, refill)
                        .await
                    {
                        Ok(ToolResult::success(format!(
                            "Per-chat resend limiter set to burst {burst} and {refill} refills \
                             per minute. {}",
                            if burst == 0 {
                                "A burst of 0 disables the limiter."
                            } else {
                                "Takes effect on each chat's next retry."
                            }
                        )))
                    } else {
                        Ok(ToolResult::error(NOT_CONNECTED.to_string()))
                    }
                }
                "set_skip_history_sync" => {
                    let enabled = pget!(get_bool(&input, "debug_enabled"));
                    if self.whatsapp_state.set_skip_history_sync(enabled).await {
                        Ok(ToolResult::success(format!(
                            "History-sync skipping is now {}.",
                            if enabled { "on" } else { "off" }
                        )))
                    } else {
                        Ok(ToolResult::error(NOT_CONNECTED.to_string()))
                    }
                }
                "set_wanted_pre_key_count" => {
                    let count = u32_param(&input, "pre_key_count") as usize;
                    if self.whatsapp_state.set_wanted_pre_key_count(count).await {
                        Ok(ToolResult::success(format!(
                            "Pre-key upload batch size set to {count}; it takes effect on the \
                             next upload."
                        )))
                    } else {
                        Ok(ToolResult::error(NOT_CONNECTED.to_string()))
                    }
                }
                "set_retry_admission" => {
                    let burst = u32_param(&input, "burst");
                    let refill = u32_param(&input, "refill_per_min");
                    let policy = std::sync::Arc::new(RetryBucket::new(burst, refill));
                    match self.whatsapp_state.set_retry_admission(policy).await {
                        Some(true) => Ok(ToolResult::success(format!(
                            "Retry admission is now bounded per chat: burst {burst}, {refill} \
                             refills per minute."
                        ))),
                        Some(false) => Ok(ToolResult::error(
                            "A retry-admission policy is already registered for this session, \
                             and the client ignores a second one, so this call changed nothing."
                                .to_string(),
                        )),
                        None => Ok(ToolResult::error(NOT_CONNECTED.to_string())),
                    }
                }
                other => Ok(ToolResult::error(format!(
                    "Unknown 'debug_op' value '{other}'. Expected one of: stats, wait_for_node, \
                     wait_for_sent_node, acquire_raw_node, release_raw_node, \
                     set_resend_rate_limit, set_skip_history_sync, set_wanted_pre_key_count, \
                     set_retry_admission."
                ))),
            },

            unknown => Ok(ToolResult::error(format!(
                "Unknown action '{}'. Valid actions: send, reply, delete, send_photo, \
                 send_document, send_audio, send_video, send_sticker, send_location, \
                 send_contact, react, send_poll, typing, mark_read, block_contact, \
                 unblock_contact, list_blocked, pin, unpin, forward, set_profile_name, \
                 set_profile_status, set_profile_picture, remove_profile_picture, \
                 status_update, list_newsletters, create_label, \
                 assign_label, send_buttons, create_group, leave_group, set_group_subject, \
                 set_group_description, add_participants, remove_participants, \
                 promote_participants, demote_participants, get_group_metadata, \
                 get_invite_link, join_with_invite_code, set_announce, set_locked, \
                 set_ephemeral, set_membership_approval, set_member_add_mode, \
                 set_limit_sharing, set_group_history, set_no_frequently_forwarded, \
                 set_allow_admin_reports, get_membership_requests, \
                 approve_membership_requests, reject_membership_requests, \
                 cancel_membership_requests, get_participating, is_admin, \
                 get_profile_picture, get_group_profile_picture, is_on_whatsapp, \
                 get_user_info, archive_chat, unarchive_chat, mute_chat, unmute_chat, \
                 star_message, unstar_message, clear_chat, save_contact, set_available, \
                 set_unavailable, subscribe_presence, unsubscribe_presence, create_quiz, \
                 vote, decrypt_vote, aggregate_votes, newsletter_get_metadata, \
                 newsletter_get_metadata_by_invite, newsletter_join, newsletter_leave, \
                 newsletter_create, newsletter_update, newsletter_revoke_message, \
                 newsletter_get_messages, community_create, community_deactivate, \
                 community_create_subgroup, community_link_subgroups, \
                 community_unlink_subgroups, community_join_subgroup, \
                 community_remove_participants, community_get_participating, \
                 community_get_subgroups, community_get_subgroup_participant_counts, \
                 community_query_linked_group, community_get_linked_groups_participants, create_event, respond_event, post_comment, wa_introspect",
                unknown
            ))),
        }
    }
}
