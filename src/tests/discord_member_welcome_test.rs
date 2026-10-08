//! FR-004 / NFR-001: welcoming a member who joins, and reporting the one
//! owner-side toggle the feature needs.
//!
//! `guild_member_addition` is delivered only while the bot holds
//! `GUILD_MEMBERS`, a privileged intent. Two failures follow from that, and
//! neither shows up in a green test run:
//!
//! 1. The Portal toggle is off, Discord refuses the IDENTIFY, and the reconnect
//!    loop retries forever on a bot that looks alive and answers nothing.
//! 2. The template renders without the mention, so the newcomer is never
//!    notified at all.
//!
//! There is no gateway in CI, and `serenity::Error` is `#[non_exhaustive]` so
//! its variants cannot be built outside the crate. The checks are split to
//! match: the pure functions are tested directly, and the wiring is pinned in
//! the source, the same way `discord_presence_test.rs` pins the presence hook.

use crate::channels::discord::member_events::{
    MISSING_TOGGLE_HINT, refused_identify_text, render_welcome,
};
use crate::config::DiscordConfig;

const AGENT: &str = include_str!("../channels/discord/agent.rs");
const MEMBER_EVENTS: &str = include_str!("../channels/discord/member_events.rs");

/// The source with every run of whitespace collapsed to a single space, so a
/// check for a call is not defeated by where `rustfmt` chose to break the line.
fn flat(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn the_template_expands_the_mention_and_the_name() {
    assert_eq!(
        render_welcome("Welcome {user} ({name})!", "<@42>", "Adi"),
        "Welcome <@42> (Adi)!",
        "FR-004: the user placeholder must expand to a real mention, because \
         Discord only notifies a member who is mentioned, and the name \
         placeholder must reach the display name"
    );
}

#[test]
fn a_template_without_placeholders_is_posted_verbatim() {
    assert_eq!(
        render_welcome("Welcome aboard!", "<@42>", "Adi"),
        "Welcome aboard!",
        "a template that names neither placeholder is a valid, if less useful, \
         greeting and must not be rewritten"
    );
}

#[test]
fn a_refused_identify_is_recognised_from_what_serenity_renders() {
    // These are the exact strings `GatewayError` renders (serenity 0.12,
    // src/gateway/error.rs:69-71), so the text fallback is grounded in the
    // dependency rather than in a guess about its wording.
    for rendered in [
        "Disallowed gateway intents were provided",
        "Invalid gateway intents were provided",
    ] {
        assert!(
            refused_identify_text(rendered),
            "NFR-001: {rendered:?} is Discord refusing the IDENTIFY over an \
             intent, and must not be treated as a transient drop"
        );
    }
}

#[test]
fn a_transient_drop_is_not_a_missing_toggle() {
    // The negative case carries as much weight as the positive one: stopping
    // the reconnect loop on an ordinary network drop would turn a hiccup into a
    // dead bot.
    for rendered in [
        "Connection closed",
        "Failed to Reconnect",
        "Sent invalid authentication",
        "Sent no authentication",
    ] {
        assert!(
            !refused_identify_text(rendered),
            "{rendered:?} is not an intent refusal, so the loop must keep \
             retrying instead of exiting"
        );
    }
}

#[test]
fn the_hint_names_the_toggle_and_where_to_flip_it() {
    for needle in [
        "GUILD_MEMBERS",
        "Developer Portal",
        "Privileged Gateway Intents",
    ] {
        assert!(
            MISSING_TOGGLE_HINT.contains(needle),
            "NFR-001: the operator has to be able to act on this line, and it \
             never names {needle:?}"
        );
    }
}

#[test]
fn the_join_handler_is_wired_with_its_delivering_intent() {
    let agent = flat(AGENT);

    assert!(
        agent.contains("async fn guild_member_addition(&self, ctx: Context, new_member: Member)"),
        "FR-004: agent.rs must wire guild_member_addition. Requesting the \
         intent without the handler subscribes to events nothing consumes"
    );
    assert!(
        agent.contains("GatewayIntents::GUILD_MEMBERS"),
        "FR-004: the handler is only delivered while GUILD_MEMBERS is \
         requested, which is exactly the defect FR-001 shipped with for \
         reactions: a handler that compiles, passes its tests and never fires"
    );
    assert!(
        agent.contains("super::member_events::handle_member_addition("),
        "the handler must delegate to the welcome module rather than grow the \
         greeting inline, so the behaviour stays reachable without a gateway"
    );
}

#[test]
fn the_reconnect_loop_stops_on_a_refused_identify() {
    let agent = flat(AGENT);
    let start = agent
        .find("refused_identify(&e)")
        .expect("agent.rs must consult member_events::refused_identify on a start() error");
    let sleep = agent[start..]
        .find("tokio::time::sleep(")
        .expect("the reconnect loop must still back off before rebuilding the client");
    let branch = &agent[start..start + sleep];

    assert!(
        branch.contains("return;"),
        "NFR-001: the refused-IDENTIFY branch must return BEFORE the sleep. \
         Sleeping and retrying reconnects every 5 seconds forever on a Portal \
         toggle only an operator can flip, which is the silent failure this \
         check exists to remove"
    );
    assert!(
        branch.contains("MISSING_TOGGLE_HINT"),
        "the fatal branch must print the hint that names the missing toggle"
    );
}

#[test]
fn a_join_posts_exactly_one_governed_message() {
    // `writes::say(` carries no whitespace, so the raw source can be counted
    // without flattening it first.
    let writes = MEMBER_EVENTS.matches("writes::say(").count();

    assert_eq!(
        writes, 1,
        "FR-004: a join posts exactly ONE message, and posts it through the \
         governor so the rate-limit ladder sees it. Found {writes} writes::say \
         calls in member_events.rs"
    );
    assert!(
        MEMBER_EVENTS.contains("Class::Final"),
        "the greeting must be Final: a welcome dropped by the write budget is \
         the silent failure NFR-001 forbids"
    );
}

#[test]
fn the_welcome_is_off_until_it_is_configured() {
    let default = DiscordConfig::default();
    assert!(
        default.welcome_message.is_none(),
        "FR-004: an install that never sets a template must post nothing, which \
         is what makes this safe to ship to every existing bot"
    );
    assert!(
        default.welcome_channel.is_none(),
        "an unset channel falls back to the guild's own system channel"
    );

    let configured: DiscordConfig = toml::from_str("welcome_message = \"Hi {user}\"\n")
        .expect("a [channels.discord] section carrying welcome_message must parse");
    assert_eq!(
        configured.welcome_message.as_deref(),
        Some("Hi {user}"),
        "the template must survive deserialization unchanged: it is expanded at \
         send time, not at load time"
    );
}

#[test]
fn the_scan_sees_the_sources_it_governs() {
    // A source scan that reads an emptied or renamed file passes every
    // assertion above for the wrong reason, so pin the anchors it depends on.
    assert!(
        AGENT.contains("impl EventHandler for Handler"),
        "agent.rs must still hold the gateway handler impl, or the wiring \
         checks above are reading nothing"
    );
    assert!(
        MEMBER_EVENTS.contains("pub(crate) async fn handle_member_addition(")
            && MEMBER_EVENTS.contains("pub(crate) fn refused_identify("),
        "member_events.rs must still hold the join handler and the intent check; \
         if they moved, move this test with them"
    );
}
