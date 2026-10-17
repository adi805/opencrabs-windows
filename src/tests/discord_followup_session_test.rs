//! Regression tests for Discord follow-up session affinity (#598).
//!
//! A tapped suggestion must answer the question that rendered the buttons.
//! The tap used to re-derive its session from the channel
//! (`discord-{channel_id}` / `discord-dm-{user}`), so an idle rotation
//! between render and tap dropped the tap into a FRESH session holding none
//! of the context the suggestion belonged to. Telegram solved this in #1217
//! by stashing the session alongside the options and never reading it from
//! client callback data; these guards hold Discord to the same contract.
//!
//! Structural guards, the same way `discord_followup_tap_tool_loop_test.rs`
//! holds #1852.

/// The renderer must stash the OWNING session with the options, not just the
/// option text.
#[test]
fn renderer_stashes_the_owning_session_with_the_options() {
    let src = include_str!("../channels/discord/suggest_options.rs");
    let at = src
        .find("register_followup(")
        .expect("#598: the renderer must stash the session with the options");
    let tail = &src[at..];
    let call = &tail[..tail.find("await").expect("the stash call is awaited")];
    assert!(
        call.contains("session_id"),
        "#598: the owning session must travel with the stashed options"
    );
    assert!(
        !src.contains("register_select("),
        "the follow-up buttons must not share the select-menu map, which has \
         no room for a session id"
    );
}

/// The tap must resolve its session from the stash entry, never from the
/// channel.
#[test]
fn tap_resolves_its_session_from_the_stash() {
    let agent = include_str!("../channels/discord/agent.rs");
    let start = agent
        .find("FOLLOWUP_PREFIX)")
        .expect("follow-up tap branch present");
    let rest = &agent[start..];
    let end = rest
        .find("// Select menu pick (#382)")
        .expect("select-menu branch terminates the tap branch");
    let branch = &rest[..end];

    assert!(
        branch.contains("take_followup("),
        "#598: the tap must consume the session-bearing follow-up entry"
    );
    assert!(
        !branch.contains("take_select("),
        "the tap must not read the session-less select map"
    );
    assert!(
        branch.contains("Some(owner_session)"),
        "#598: the stashed session must reach the turn as an override"
    );
}

/// The turn must honour the override before falling back to the channel.
#[test]
fn turn_prefers_the_override_over_the_channel() {
    let interactions = include_str!("../channels/discord/interactions.rs");
    let start = interactions
        .find("pub(crate) async fn route_followup_turn(")
        .expect("tap-turn helper present");
    let body = &interactions[start..];

    assert!(
        body.contains("session_override: Option<Uuid>"),
        "#598: the tap-turn helper must accept a session override"
    );
    let arm = body
        .find("match session_override")
        .expect("#598: the override must be matched before resolving a session");
    let fallback = body
        .find("resolve_interaction_session(")
        .expect("the channel-derived resolution must survive for slash commands");
    assert!(
        arm < fallback,
        "the override must be consulted BEFORE the channel-derived resolution"
    );
    assert!(
        body.contains("Some(sid) => sid,"),
        "#598: an override must be used verbatim, not re-resolved"
    );
}

/// A slash command has no rendering session to inherit, so its override stays
/// `None` and the channel resolution stands (FR-002 is unchanged).
#[test]
fn slash_arm_keeps_the_channel_derived_session() {
    let interactions = include_str!("../channels/discord/interactions.rs");
    let helper_start = interactions
        .find("pub(crate) async fn handle_invoked_request(")
        .expect("the shared invoked-request helper must exist");
    let helper_rest = &interactions[helper_start..];
    let helper_end = helper_rest
        .find("pub(crate) async fn route_followup_turn(")
        .expect("route_followup_turn terminates the invoked-request helper");
    let helper = &helper_rest[..helper_end];
    let call_start = helper
        .find("route_followup_turn(")
        .expect("the slash arm dispatches through the tap helper");
    let call = &helper[call_start..];
    let args = &call[..call.find(".await").expect("the slash call is awaited")];
    // Strip the call's own closing paren before reading the final argument.
    let args = args.trim_end().trim_end_matches(')').trim_end();
    assert!(
        args.ends_with("None,"),
        "#598: a slash command has no rendering session to inherit, so the \
         override must be None and the channel resolution must stand"
    );
}

/// The stash entry is TTL-bounded and single-use, exactly like the select map
/// it no longer shares.
#[test]
fn the_followup_stash_is_ttl_bounded_and_single_use() {
    let src = include_str!("../channels/discord/pending_interactions.rs");
    let at = src
        .find("async fn take_followup(")
        .expect("#598: the session-bearing take must exist");
    let tail = &src[at..];
    let body = &tail[..tail.find("async fn register_form(").unwrap_or(tail.len())];
    assert!(
        body.contains("expired("),
        "a stale suggestion must answer expired, not fire a stale turn"
    );
    assert!(
        body.contains("map.remove(id)"),
        "a taken follow-up must be consumed so a double tap cannot replay it"
    );
    assert!(
        body.contains("-> Option<(Uuid, Vec<String>)>"),
        "the take must hand back the session it was stashed under"
    );
}
