//! Right-click context menus for Discord (FR-006 / AC-009).
//!
//! Two of these tests read source rather than calling it, because CI has no
//! Discord application and no gateway to hand an interaction to. What cannot be
//! exercised is pinned by reading the files that carry it: a doc comment cannot
//! fail a build, so the wiring is asserted where it lives.
//!
//! Scope, stated so a later reader does not widen or narrow it: the registration
//! of the two menus, the request text a right-click turns into, and the three
//! invariants that are invisible until they break (one global overwrite, one
//! access gate, one dispatch arm). It does not cover the gateway handshake,
//! which needs a live Discord.

use std::path::Path;

use crate::channels::discord::commands::{sanitize_name, sync_signature};
use crate::channels::discord::context_menu::{
    ENTRIES, MESSAGE_COMMAND, NAME_MAX, USER_COMMAND, commands, message_prompt, signature,
    user_prompt,
};
use serenity::model::application::CommandType;

/// Read a file from the crate root.
fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Whitespace-insensitive view, so a pin does not break on a reflow.
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Every `.rs` file in the discord channel module, sorted by name.
fn discord_sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels/discord");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read src/channels/discord") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        out.push((name, std::fs::read_to_string(&path).expect("read source")));
    }
    out.sort();
    out
}

#[test]
fn the_two_menus_are_registered_with_discords_own_types() {
    // `CommandType::User` (2) and `CommandType::Message` (3) are the two
    // right-click surfaces. `ChatInput` (1) is the `/` menu and is not this.
    assert_eq!(ENTRIES.len(), 2, "one message menu and one user menu");
    assert_eq!(ENTRIES[0], (MESSAGE_COMMAND, CommandType::Message));
    assert_eq!(ENTRIES[1], (USER_COMMAND, CommandType::User));

    let built = commands();
    assert_eq!(built.len(), ENTRIES.len());

    let message = serde_json::to_value(&built[0]).expect("serialize the message menu");
    assert_eq!(message["name"], MESSAGE_COMMAND);
    assert_eq!(message["type"], 3, "MESSAGE");

    let user = serde_json::to_value(&built[1]).expect("serialize the user menu");
    assert_eq!(user["name"], USER_COMMAND);
    assert_eq!(user["type"], 2, "USER");
}

#[test]
fn a_context_menu_carries_no_description_and_no_options() {
    // Discord requires a description of `CHAT_INPUT` alone: "Empty string for
    // USER and MESSAGE commands" (`application-commands.mdx:62`), and its own
    // USER example sends name and type and nothing else (`:286`). Serenity omits
    // a field that was never set, so the right move is to leave it unset.
    for builder in commands() {
        let wire = serde_json::to_value(&builder).expect("serialize");
        let object = wire.as_object().expect("a command serializes to an object");
        let kind = object.get("type").and_then(serde_json::Value::as_u64);
        assert!(
            matches!(kind, Some(2) | Some(3)),
            "a context menu is USER (2) or MESSAGE (3), got {kind:?}"
        );
        assert!(
            !object.contains_key("description"),
            "a context menu must not carry a description: {wire}"
        );
        // Serenity always emits the field; it must be empty rather than absent
        // so a reader does not think an argument was forgotten.
        assert_eq!(
            wire["options"].as_array().map(Vec::len),
            Some(0),
            "a context menu takes no options: {wire}"
        );
    }
}

#[test]
fn the_menu_names_are_legal_and_must_not_be_sanitized() {
    // "USER and MESSAGE commands may be mixed case and can include spaces"
    // (`application-commands.mdx:49`), unlike `CHAT_INPUT`'s `^[\w-]{1,32}$`.
    for (name, _) in ENTRIES {
        let len = name.chars().count();
        assert!((1..=NAME_MAX).contains(&len), "{name:?} is {len} chars");
        assert!(name.contains(' '), "{name:?} should read as a label");
    }
    // The regression this pins: `sanitize_name` is for `CHAT_INPUT` and would
    // rewrite "Ask agent" to "ask-agent". Route these names through it and the
    // menu silently stops matching what is documented.
    assert_ne!(
        sanitize_name(MESSAGE_COMMAND).as_deref(),
        Some(MESSAGE_COMMAND)
    );
    assert_ne!(sanitize_name(USER_COMMAND).as_deref(), Some(USER_COMMAND));
}

#[test]
fn a_message_request_carries_the_author_the_link_and_the_text_verbatim() {
    let text = message_prompt(
        "Adi",
        42,
        "https://discord.com/channels/1/2/3",
        "why is CI red",
    );
    // Verbatim, not summarised: the agent is being asked about THIS message,
    // and a paraphrase would answer about a message nobody sent.
    assert!(text.contains("why is CI red"), "{text}");
    // The id travels with the name: a display name is not stable and two
    // members can share one.
    assert!(text.contains("Adi (42)"), "{text}");
    assert!(
        text.contains("https://discord.com/channels/1/2/3"),
        "{text}"
    );
}

#[test]
fn a_message_with_no_text_says_so_instead_of_looking_truncated() {
    // An attachment-only or embed-only message has empty content. The agent
    // cannot open a Discord link, so the honest signal is that there was no
    // text rather than a blank block it might fill in from context.
    for empty in ["", "   ", "\n\t"] {
        let text = message_prompt("Adi", 42, "https://x/1/2/3", empty);
        assert!(text.contains("(no text content)"), "{empty:?} -> {text}");
    }
}

#[test]
fn a_user_request_carries_the_id_because_a_name_is_not_stable() {
    let text = user_prompt("Adi", 7);
    assert!(
        text.contains("Name: Adi"),
        "the name is for readability: {text}"
    );
    assert!(text.contains("Id: 7"), "the id is the payload: {text}");
}

#[test]
fn the_sync_key_moves_when_either_component_moves() {
    // The key folds both components, so an edit to either one re-syncs.
    let catalog = 0xAAAA_u64;
    let menus = signature();
    let combined = sync_signature(&[catalog, menus]);
    assert_eq!(combined, sync_signature(&[catalog, menus]), "deterministic");
    // A changed menu set must not land on the same key, or the edit is skipped
    // and the stale menus stay registered.
    assert_ne!(combined, sync_signature(&[catalog, menus ^ 0xFFFF]));
    // Sequential hashing, not XOR: XOR is its own inverse, so two components
    // that swapped places would collide on the same key.
    assert_ne!(sync_signature(&[1, 2]), sync_signature(&[2, 1]));
    assert_eq!(signature(), signature(), "the registered set is stable");
}

#[test]
fn the_menus_travel_in_the_same_overwrite_as_the_catalog() {
    let src = read("src/channels/discord/commands.rs");
    // ONE PUT. `set_global_commands` replaces the whole global set, so a second
    // call would erase the catalog and the next catalog sync would erase the
    // menus. The module doc says so; this pins the call count.
    assert_eq!(
        src.matches("Command::set_global_commands(").count(),
        1,
        "the whole command set goes in one overwrite"
    );
    let folded = flat(&src);
    assert!(
        folded.contains("super::context_menu::commands()"),
        "the menus join the payload"
    );
    assert!(
        folded.contains("sync_signature(&[plan.signature(),super::context_menu::signature()])"),
        "both components feed the sync key"
    );
    // The extend must precede the call, or the menus are built and dropped.
    let extend = src.find("commands.extend(menus)").expect("the extend");
    let put = src.find("Command::set_global_commands(").expect("the PUT");
    assert!(
        extend < put,
        "the extend at {extend} must precede the PUT at {put}"
    );
}

#[test]
fn a_context_menu_is_dispatched_by_its_target_type() {
    let src = read("src/channels/discord/context_menu.rs");
    // `?` on `target_id` is the trap: Discord omits a target it did not resolve,
    // which is how a message deleted between the right-click and the
    // interaction landing arrives.
    assert!(
        src.contains("let target = command.data.target_id?;"),
        "the target is the entry point"
    );
    assert!(src.contains("CommandType::Message => {"));
    assert!(src.contains("CommandType::User => {"));
    assert!(src.contains(".get(&target.to_message_id())?;"));
    assert!(src.contains("command.data.resolved.users.get(&target.to_user_id())?;"));
}

#[test]
fn a_picked_command_rebuilds_the_text_a_typed_message_would_have_had() {
    // No gateway in CI, so the shape is pinned: the invocation is `/name` plus
    // the one `args` option this channel registers, which is what makes a
    // picked command and a typed message the same request. An option that is
    // not `args` is logged rather than ignored, because one is all we register.
    let src = read("src/channels/discord/commands.rs");
    assert!(src.contains("let mut text = format!(\"/{}\", command.data.name);"));
    assert!(src.contains("option.name == ARGS_OPTION"));
    assert!(src.contains("text.push_str(value.as_str());"));
    assert!(src.contains("} else if option.name != ARGS_OPTION {"));
}

#[test]
fn an_unreadable_target_is_refused_in_place_and_never_panicked() {
    let src = read("src/channels/discord/agent.rs");
    let folded = flat(&src);
    // A right-click routes to the context-menu request builder, and a picked
    // command to the catalog one: one arm, two sources of text.
    assert!(folded.contains("CommandType::Message|CommandType::User=>{"));
    assert!(folded.contains("super::context_menu::invocation(command)"));
    assert!(folded.contains("CommandType::ChatInput=>Some(super::commands::invocation(command))"));
    // A type we never registered is refused rather than guessed at.
    assert!(
        folded.contains("_=>None,"),
        "an unknown kind has no request"
    );
    // `None` is answered ephemeral, in place: a refused interaction beats the
    // red "didn't respond" banner, and running a turn on a message nobody can
    // see is worse than both.
    assert!(folded.contains("letSome(request)=requestelse{"));
    assert!(
        src.contains("Nothing to ask about"),
        "the refusal is user-facing"
    );
    assert!(folded.contains(".ephemeral(true)"));
}

#[test]
fn every_interaction_entry_point_shares_one_gate_and_one_dispatch() {
    // The OC-02 deny-by-default gate is the access control for an invoked
    // interaction, and a second copy is the copy that gets forgotten the next
    // time the rule moves. The message path keeps its own inline copy in
    // `handler.rs`; inside the interaction path there must be exactly one.
    let mut definitions = 0usize;
    let mut call_sites = 0usize;
    for (name, src) in discord_sources() {
        for line in src.lines() {
            let hits = line.matches("identity_admitted(").count();
            if hits == 0 {
                continue;
            }
            if line.contains("fn identity_admitted(") {
                definitions += hits;
            } else {
                call_sites += hits;
                assert_eq!(
                    name, "interactions.rs",
                    "a second copy of the gate appeared in {name}"
                );
            }
        }
    }
    assert_eq!(definitions, 1, "the rule has one definition");
    assert_eq!(
        call_sites, 1,
        "the rule has one call site in the interaction path"
    );

    // And both interaction kinds share that one call site: one dispatch
    // function, declared once, called once.
    let interactions = read("src/channels/discord/interactions.rs");
    assert_eq!(
        interactions
            .matches("pub(crate) async fn handle_invoked_request(")
            .count(),
        1,
        "one dispatch function"
    );
    let agent = read("src/channels/discord/agent.rs");
    assert_eq!(
        agent
            .matches("super::interactions::handle_invoked_request(")
            .count(),
        1,
        "one arm dispatches both a picked command and a right-click"
    );
}
