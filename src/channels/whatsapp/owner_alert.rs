//! Cross-channel owner alerts for WhatsApp account events (#1999).
//!
//! A ban, an account lock or a connect failure takes WhatsApp down, so the
//! channel that is down cannot be the channel that reports it. Alerts ride
//! Telegram instead: a different transport, a different credential, and one
//! that is up whenever this process is.
//!
//! Two properties are load-bearing here and neither is optional:
//!
//! * **Best-effort.** A failed alert must never reach the event loop. Every
//!   send is a single request whose error is logged and dropped.
//! * **Bounded.** `ConnectFailure` fires on *every* reconnect attempt while a
//!   number is banned, so a cause is announced at most once per
//!   [`ALERT_COOLDOWN`] rather than once per attempt.
//!
//! Only account events that need a human are routed here. Transient connect
//! failures the client retries by itself are logged, not announced: see
//! [`connect_failure_needs_owner`].

use crate::config::Config;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use wacore::types::events::{ConnectFailureReason, TempBanReason};

/// Quiet period per distinct alert cause. A ban is announced once, not once
/// per reconnect attempt.
pub(crate) const ALERT_COOLDOWN: Duration = Duration::from_secs(15 * 60);

/// Dedup state: alert key -> when it was last announced.
fn announced() -> &'static Mutex<HashMap<String, Instant>> {
    static ANNOUNCED: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    ANNOUNCED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Claim the right to announce `key`. `true` means this caller owns the
/// announcement; `false` means the same cause was announced within
/// [`ALERT_COOLDOWN`] and must stay quiet.
pub(crate) fn claim_alert(key: &str, now: Instant) -> bool {
    let mut seen = announced().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = seen.get(key)
        && now.saturating_duration_since(*at) < ALERT_COOLDOWN
    {
        return false;
    }
    seen.insert(key.to_owned(), now);
    true
}

/// Telegram DM chat that receives owner alerts.
///
/// A private chat's id *is* the user id, so an explicit `bot_owner` addresses
/// the operator directly; when it is unset the first allow-list entry is the
/// operator who set the channel up, matching `config::owner::is_owner`'s
/// positional fallback.
pub(crate) fn owner_alert_chat(cfg: &Config) -> Option<i64> {
    let telegram = &cfg.channels.telegram;
    if !telegram.enabled {
        return None;
    }
    telegram
        .bot_owner
        .iter()
        .chain(telegram.allowed_users.iter())
        .find_map(|id| id.trim_start_matches('+').parse::<i64>().ok())
}

/// Compact human duration: `2d 4h`, `3h 20m`, `45m`.
///
/// The wire sends a count of seconds, and an operator cannot act on `2700` at
/// a glance.
pub(crate) fn human_duration(total: Duration) -> String {
    let mut secs = total.as_secs();
    let mut parts: Vec<String> = Vec::new();
    for (unit, secs_per) in [("d", 86_400_u64), ("h", 3_600), ("m", 60)] {
        let value = secs / secs_per;
        if value > 0 {
            parts.push(format!("{value}{unit}"));
            secs %= secs_per;
        }
    }
    if parts.is_empty() {
        parts.push("under a minute".to_owned());
    }
    parts.join(" ")
}

/// Operator-facing name for a connect failure.
///
/// The library ships no `Display` for this enum, and a bare `402` tells an
/// operator nothing about what to do next, so every code that changes the
/// response gets a phrase.
pub(crate) fn connect_failure_name(reason: &ConnectFailureReason) -> String {
    use ConnectFailureReason as R;
    let name = match reason {
        R::Generic => "generic connect failure",
        R::LoggedOut => "logged out",
        R::TempBanned => "temporarily banned",
        R::AccountLocked => "account locked",
        R::UnknownLogout => "logged out, reason not recognised",
        R::ClientOutdated => "client version no longer accepted",
        R::BadUserAgent => "user agent rejected",
        R::CatExpired => "client token expired",
        R::CatInvalid => "client token invalid",
        R::NotFound => "client no longer known to the server",
        R::ClientUnknown => "client unknown to the server",
        R::InternalServerError => "server error",
        R::Experimental => "experimental client version rejected",
        R::ServiceUnavailable => "service unavailable",
        R::Unknown(code) => return format!("unrecognised connect failure (wire {code})"),
    };
    name.to_owned()
}

/// Whether a connect failure is something the operator must act on.
///
/// Transient reasons are retried by the client itself, so logging them is
/// enough; an alert per reconnect attempt would be noise rather than signal.
pub(crate) fn connect_failure_needs_owner(reason: &ConnectFailureReason) -> bool {
    !reason.should_reconnect()
}

/// Text for `Event::TemporaryBan`.
pub(crate) fn ban_text(
    code: &TempBanReason,
    expire: Duration,
    message: Option<&str>,
    url: Option<&str>,
) -> String {
    let duration = human_duration(expire);
    let mut lines = vec![
        "WhatsApp: account temporarily banned".to_owned(),
        format!("Reason: {code}"),
        format!("Duration: {duration}"),
        "The channel cannot send until this expires.".to_owned(),
    ];
    if let Some(message) = message.filter(|m| !m.trim().is_empty()) {
        lines.push(format!("Server: {message}"));
    }
    if let Some(url) = url.filter(|u| !u.trim().is_empty()) {
        lines.push(format!("Appeal: {url}"));
    }
    lines.join("\n")
}

/// Text for `Event::ConnectFailure`.
pub(crate) fn connect_failure_text(reason: &ConnectFailureReason, message: Option<&str>) -> String {
    let name = connect_failure_name(reason);
    let mut lines = vec![
        format!("WhatsApp: connection refused [{}]", reason.code()),
        format!("Reason: {name}"),
    ];
    if let Some(message) = message.filter(|m| !m.trim().is_empty()) {
        lines.push(format!("Server: {message}"));
    }
    lines.join("\n")
}

/// Text for `Event::LoggedOut`.
///
/// The distinction this message exists to carry: a server-side account lock
/// (403) is not a voluntary unlink. Reading one as the other sends the operator
/// the wrong way, and the one-time appeal data in the stanza is gone either
/// way.
pub(crate) fn logged_out_text(
    on_connect: bool,
    reason: &ConnectFailureReason,
    header: Option<&str>,
    subtext: Option<&str>,
) -> String {
    let when = if on_connect {
        "at connect"
    } else {
        "mid-session"
    };
    let name = connect_failure_name(reason);
    let mut lines = vec![
        format!("WhatsApp: session ended ({when})"),
        format!("Reason: {name} [{}]", reason.code()),
    ];
    match reason {
        ConnectFailureReason::AccountLocked => lines.push(
            "The account is locked server-side, which is not the same event as a manual unlink. \
             Do not re-pair until you know which one this was."
                .to_owned(),
        ),
        ConnectFailureReason::UnknownLogout => lines.push(
            "The server ended the session without a reason this client recognises.".to_owned(),
        ),
        _ => {}
    }
    if let Some(header) = header.filter(|h| !h.trim().is_empty()) {
        lines.push(format!("Server: {header}"));
    }
    if let Some(subtext) = subtext.filter(|s| !s.trim().is_empty()) {
        lines.push(subtext.to_owned());
    }
    lines.join("\n")
}

/// Announce `text` to the owner over Telegram.
///
/// Best-effort by contract: one request, the error logged and dropped, and
/// `false` when nothing was delivered. Callers must not branch on the result in
/// a way that can stall the event loop.
pub(crate) async fn alert_owner(text: &str) -> bool {
    #[cfg(not(feature = "telegram"))]
    {
        tracing::warn!(
            "WhatsApp: owner alert not delivered (telegram feature disabled): {}",
            text.lines().next().unwrap_or(text)
        );
        false
    }

    #[cfg(feature = "telegram")]
    {
        let cfg = Config::current();
        let Some(chat) = owner_alert_chat(&cfg) else {
            tracing::warn!(
                "WhatsApp: owner alert not delivered, no Telegram owner chat is configured \
                 (enable channels.telegram and set bot_owner or allowed_users)"
            );
            return false;
        };
        let Some(token) = cfg
            .channels
            .telegram
            .token
            .as_deref()
            .filter(|t| !t.is_empty())
        else {
            tracing::warn!("WhatsApp: owner alert not delivered, channels.telegram has no token");
            return false;
        };
        let bot = teloxide::Bot::new(token);
        let chat = teloxide::types::ChatId(chat);
        match crate::channels::telegram::send::message_in_thread(&bot, chat, None, text).await {
            Ok(sent) => {
                tracing::info!(
                    chat = chat.0,
                    message_id = sent.id.0,
                    "WhatsApp: owner alert delivered"
                );
                true
            }
            Err(e) => {
                tracing::warn!("WhatsApp: owner alert failed: {e}");
                false
            }
        }
    }
}
