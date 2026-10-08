//! FR-001 / NFR-003 / AC-019: a wired handler must have its delivering intent.
//!
//! `src/channels/discord/agent.rs` wires four `EventHandler` methods, and the
//! gateway only delivers an event whose intent was requested at connect time.
//! Those two facts live in different places in the same file, so a handler can
//! be wired, compile, pass every behavioural test, and still never fire. That
//! is the FR-001 defect: `reaction_add` sat dead because
//! `GUILD_MESSAGE_REACTIONS | DIRECT_MESSAGE_REACTIONS` were never requested.
//!
//! A doc comment cannot fail a build, so this test reads the source and turns
//! the pairing into a build failure. Two rules:
//!
//! - Every wired handler must have at least one of the intents that deliver
//!   its event present in the requested set.
//! - Every wired handler must be classified in this file, so wiring a new one
//!   forces its author to state which intent delivers it.
//!
//! Extraction is line-based on purpose: the impl header sits at column 0 and
//! rustfmt indents everything inside it, so the first column-0 `}` after the
//! header closes the block. Brace counting would be fooled by the `{x}` inside
//! the `format!` calls in that same body.

use std::path::Path;

/// Handler -> the intents that deliver its event. Discord sends the event when
/// ANY one of them is requested, which is why the guild and DM bits are both
/// listed: a reaction in a DM arrives on `DIRECT_MESSAGE_REACTIONS`.
const DELIVERED_BY: &[(&str, &[&str])] = &[
    (
        "reaction_add",
        &["GUILD_MESSAGE_REACTIONS", "DIRECT_MESSAGE_REACTIONS"],
    ),
    (
        "reaction_remove",
        &["GUILD_MESSAGE_REACTIONS", "DIRECT_MESSAGE_REACTIONS"],
    ),
    (
        "reaction_remove_emoji",
        &["GUILD_MESSAGE_REACTIONS", "DIRECT_MESSAGE_REACTIONS"],
    ),
    (
        "reaction_remove_all",
        &["GUILD_MESSAGE_REACTIONS", "DIRECT_MESSAGE_REACTIONS"],
    ),
    ("message", &["GUILD_MESSAGES", "DIRECT_MESSAGES"]),
    ("message_update", &["GUILD_MESSAGES", "DIRECT_MESSAGES"]),
    ("message_delete", &["GUILD_MESSAGES", "DIRECT_MESSAGES"]),
    ("guild_member_addition", &["GUILD_MEMBERS"]),
    ("guild_member_removal", &["GUILD_MEMBERS"]),
    ("presence_update", &["GUILD_PRESENCES"]),
    (
        "typing_start",
        &["GUILD_MESSAGE_TYPING", "DIRECT_MESSAGE_TYPING"],
    ),
];

/// Handlers Discord delivers with no intent gate: lifecycle events and
/// interactions. Guild-shape handlers are absent because serenity always
/// requests `GUILDS`.
const NO_INTENT_NEEDED: &[&str] = &[
    "ready",
    "resume",
    "shard_ready",
    "cache_ready",
    "guild_create",
    "guild_delete",
    "guild_update",
    "channel_create",
    "channel_delete",
    "interaction_create",
];

fn agent_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels/discord/agent.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The `impl EventHandler for Handler` body: header line to the first column-0
/// `}` after it.
fn event_handler_body(src: &str) -> String {
    let header = src
        .lines()
        .position(|line| line.starts_with("impl EventHandler for Handler"))
        .unwrap_or_else(|| panic!("agent.rs no longer has `impl EventHandler for Handler`"));
    let body: Vec<&str> = src
        .lines()
        .skip(header + 1)
        .take_while(|line| *line != "}")
        .collect();
    assert!(
        body.iter()
            .any(|l| l.trim_start().starts_with("async fn message")),
        "the impl body ended before `message`, so this scan is reading the \
         wrong region: agent.rs is no longer rustfmt-clean at the top level, \
         and a column-0 `}}` now appears inside the block"
    );
    body.join("\n")
}

/// Every `async fn` wired as a handler, in source order.
fn wired_handlers(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("async fn ")?;
            let name = rest.split(['(', '<']).next()?;
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
        .collect()
}

/// The `GatewayIntents::*` names in the `let intents = ...;` expression.
fn requested_intents(src: &str) -> Vec<String> {
    let start = src
        .find("let intents =")
        .unwrap_or_else(|| panic!("agent.rs no longer builds a `let intents` value"));
    let end = start
        + src[start..]
            .find(';')
            .unwrap_or_else(|| panic!("unterminated `let intents` expression"));
    src[start..end]
        .match_indices("GatewayIntents::")
        .filter_map(|(i, marker)| {
            let name: String = src[start + i + marker.len()..end]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        })
        .collect()
}

#[test]
fn every_wired_handler_has_its_delivering_intent() {
    let src = agent_source();
    let intents = requested_intents(&src);
    let body = event_handler_body(&src);
    let mut offenders = Vec::new();
    for handler in wired_handlers(&body) {
        let needed = match DELIVERED_BY.iter().find(|(name, _)| *name == handler) {
            Some((_, required)) => *required,
            None => continue,
        };
        let covered = needed
            .iter()
            .any(|want| intents.iter().any(|have| have.as_str() == *want));
        if !covered {
            offenders.push(format!("  {handler}: needs one of {needed:?}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "a wired handler cannot fire because its delivering intent was never \
         requested (NFR-003 / AC-019). Requested: {intents:?}\n{}",
        offenders.join("\n")
    );
}

#[test]
fn every_wired_handler_is_classified() {
    let src = agent_source();
    let body = event_handler_body(&src);
    let unclassified: Vec<String> = wired_handlers(&body)
        .into_iter()
        .filter(|handler| {
            !DELIVERED_BY
                .iter()
                .any(|(name, _)| *name == handler.as_str())
                && !NO_INTENT_NEEDED.contains(&handler.as_str())
        })
        .collect();
    assert!(
        unclassified.is_empty(),
        "handler(s) wired without a delivering intent on record: {unclassified:?}. \
         Add each to DELIVERED_BY with the intents that deliver it, or to \
         NO_INTENT_NEEDED if Discord sends it ungated, so the pairing is a \
         decision rather than an accident."
    );
}

/// The scan must be able to fail. A path typo, a rename, or a region that
/// silently shrinks would make the two tests above pass while checking
/// nothing, which is worse than no test: it reports safety it never verified.
#[test]
fn the_scan_sees_the_handlers_it_governs() {
    let src = agent_source();
    let body = event_handler_body(&src);
    let seen = wired_handlers(&body);
    assert!(
        seen.len() >= 4,
        "expected the wired EventHandler methods, saw {seen:?}"
    );
    for expected in ["reaction_add", "ready", "message", "interaction_create"] {
        assert!(
            seen.iter().any(|h| h.as_str() == expected),
            "`{expected}` is no longer wired, so the scan would pass vacuously: {seen:?}"
        );
    }
    assert!(
        requested_intents(&src).len() >= 3,
        "the intent scan found fewer than three intents, so it is reading the \
         wrong region of agent.rs"
    );
}
