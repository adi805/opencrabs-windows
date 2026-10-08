//! Member joins: the welcome message (FR-004) plus the startup check that
//! reports a missing privileged intent (NFR-001).
//!
//! Discord delivers `guild_member_addition` only while the bot holds the
//! `GUILD_MEMBERS` intent, and that intent is gated behind an application
//! toggle the OWNER has to flip in the Developer Portal. Two failure modes
//! follow, and both are silent by default:
//!
//! 1. The intent is requested but the toggle is off. Discord answers the
//!    IDENTIFY with close code 4014 (`Disallowed intent(s)`), serenity
//!    surfaces it as `GatewayError::DisallowedGatewayIntents`, and the
//!    reconnect loop in `agent.rs` would retry it every 5 seconds forever,
//!    logging one line per attempt: a bot that looks alive, sits in the
//!    guild, answers nothing, and never says why. That is the failure NFR-001
//!    exists to kill, so [`refused_identify`] turns it into a named
//!    instruction and the loop stops.
//! 2. The toggle is on but the template is unset, which is the state of every
//!    install that predates this feature. That one is intentional and stays
//!    quiet, logged at debug so a silent channel is explainable.
//!
//! The greeting itself is one governed write per join through [`writes`], so
//! the rate-limit ladder sees it like any other channel write.

use super::writes::{self, Class};
use crate::config::{Config, DiscordConfig};
use serenity::gateway::GatewayError;
use serenity::model::guild::Member;
use serenity::model::id::ChannelId;
use serenity::prelude::Context;

/// Placeholder in `welcome_message` for the `<@id>` mention.
pub(crate) const USER_PLACEHOLDER: &str = "{user}";

/// Placeholder in `welcome_message` for the member's display name.
pub(crate) const NAME_PLACEHOLDER: &str = "{name}";

/// Expand a welcome template.
///
/// `{user}` becomes a real `<@id>` mention rather than a bare name: Discord
/// only notifies a member who is mentioned, so a template that drops the
/// mention produces a greeting the newcomer never sees.
pub(crate) fn render_welcome(template: &str, mention: &str, name: &str) -> String {
    template
        .replace(USER_PLACEHOLDER, mention)
        .replace(NAME_PLACEHOLDER, name)
}

/// A value that is present and not blank after trimming, or `None`.
fn usable(value: Option<&str>) -> Option<&str> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// The name to greet someone by: guild nickname, else global display name,
/// else username. A blank value at either step falls through, so a template
/// using the name placeholder cannot render a blank where a name belongs.
fn display_name(member: &Member) -> &str {
    if let Some(nick) = usable(member.nick.as_deref()) {
        return nick;
    }

    let global = usable(member.user.global_name.as_deref());
    global.unwrap_or(member.user.name.as_str())
}

/// The configured welcome channel, or `None` to fall back to the guild's
/// system channel.
///
/// A value that is present but not a snowflake is a typo in config, so it is
/// logged and then treated as unset: falling back to the system channel is a
/// better outcome than dropping the greeting, but the typo still has to be
/// visible (NFR-001).
fn configured_channel(discord: &DiscordConfig) -> Option<ChannelId> {
    let configured = discord.welcome_channel.as_deref();
    let raw = configured?.trim();
    if raw.is_empty() {
        return None;
    }

    match raw.parse::<u64>() {
        Ok(id) => Some(ChannelId::new(id)),
        Err(e) => {
            tracing::warn!(
                value = raw,
                error = %e,
                "Discord: welcome_channel is not a numeric channel ID; \
                 falling back to the guild's system channel"
            );
            None
        }
    }
}

/// Greet a member who just joined a guild (FR-004).
pub(crate) async fn handle_member_addition(
    ctx: &Context,
    member: &Member,
    config_rx: tokio::sync::watch::Receiver<Config>,
) {
    // Bots join constantly (apps, integrations, other bots) and Discord's own
    // onboarding does not ping them. Greeting one is noise in the channel.
    if member.user.bot {
        tracing::debug!(
            user_id = member.user.id.get(),
            guild_id = member.guild_id.get(),
            "Discord: a bot joined the guild; no welcome"
        );
        return;
    }

    // The borrow is confined to its own block: a `watch::Ref` holds a read
    // guard and is not `Send`, so it must not live across the awaits below.
    let (template, configured) = {
        let cfg = config_rx.borrow();
        let discord = &cfg.channels.discord;
        (discord.welcome_message.clone(), configured_channel(discord))
    };

    let Some(template) = template.filter(|t| !t.trim().is_empty()) else {
        tracing::debug!(
            user_id = member.user.id.get(),
            guild_id = member.guild_id.get(),
            "Discord: member joined but welcome_message is unset; no welcome"
        );
        return;
    };

    let channel = match configured {
        Some(channel) => channel,
        None => match ctx.http.get_guild(member.guild_id).await {
            Ok(guild) => match guild.system_channel_id {
                Some(channel) => channel,
                None => {
                    tracing::warn!(
                        guild_id = member.guild_id.get(),
                        "Discord: welcome_message is set but neither welcome_channel \
                         nor the guild's system channel is available; no welcome posted"
                    );
                    return;
                }
            },
            Err(e) => {
                tracing::warn!(
                    guild_id = member.guild_id.get(),
                    error = %e,
                    "Discord: could not resolve a welcome channel; no welcome posted"
                );
                return;
            }
        },
    };

    let mention = format!("<@{}>", member.user.id.get());
    let text = render_welcome(&template, &mention, display_name(member));

    // Exactly one write per join, in the `Final` class so the greeting is
    // never dropped by the budget: a welcome that silently disappears is the
    // outcome this feature exists to prevent (NFR-001).
    match writes::say(&ctx.http, channel, &text, Class::Final).await {
        Ok(Some(msg)) => tracing::info!(
            channel_id = channel.get(),
            message_id = msg.id.get(),
            user_id = member.user.id.get(),
            "Discord: welcomed a new member"
        ),
        Ok(None) => tracing::warn!(
            channel_id = channel.get(),
            user_id = member.user.id.get(),
            "Discord: welcome write was refused by the budget governor"
        ),
        Err(e) => tracing::warn!(
            channel_id = channel.get(),
            user_id = member.user.id.get(),
            error = %e,
            "Discord: welcome message failed to send"
        ),
    }
}

/// What to print when the gateway refuses the IDENTIFY over an intent.
///
/// Names the toggle, the file, and the reason retrying cannot help: the
/// reconnect loop stops here, and an owner reading the log needs the fix, not
/// a fifth identical retry line.
pub(crate) const MISSING_TOGGLE_HINT: &str = "Discord refused the gateway IDENTIFY because a \
     requested privileged intent is not enabled on the application. Enable GUILD_MEMBERS in the \
     Developer Portal (your app -> Bot -> Privileged Gateway Intents), then restart OpenCrabs. \
     Reconnecting cannot flip a Portal toggle, so the reconnect loop stops here (NFR-001).";

/// Rendered-error substrings that mean Discord refused the IDENTIFY over a
/// privileged intent. These are the exact strings `GatewayError` renders
/// (serenity 0.12 `src/gateway/error.rs:69-71`).
const REFUSED_IDENTIFY_MARKERS: &[&str] = &[
    "Disallowed gateway intents",
    "Invalid gateway intents",
];

/// Whether a rendered gateway error names an intent refusal.
///
/// Split out from [`refused_identify`] so the text fallback is testable
/// without constructing a `serenity::Error`: both serenity enums are
/// `#[non_exhaustive]`, so only the crate can build their variants.
pub(crate) fn refused_identify_text(message: &str) -> bool {
    REFUSED_IDENTIFY_MARKERS
        .iter()
        .any(|marker| message.contains(*marker))
}

/// Whether the gateway error itself is an intent refusal.
fn is_intent_refusal(error: &GatewayError) -> bool {
    matches!(error, GatewayError::DisallowedGatewayIntents | GatewayError::InvalidGatewayIntents)
}

/// Whether `err` is Discord refusing the IDENTIFY over an intent.
///
/// Two checks on purpose: the variant match is exact, and the rendered text
/// catches an error that arrives wrapped in something this match cannot see.
pub(crate) fn refused_identify(err: &serenity::Error) -> bool {
    let rendered = err.to_string();
    if refused_identify_text(&rendered) {
        return true;
    }

    match err {
        serenity::Error::Gateway(error) => is_intent_refusal(error),
        _ => false,
    }
}
