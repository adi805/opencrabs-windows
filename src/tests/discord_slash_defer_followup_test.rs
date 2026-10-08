//! Regression tests for the Discord slash-command deferred ack (FR-002).
//!
//! FR-001 (#27) stopped the red "This interaction didn't respond" banner by
//! answering the interaction inside the 3-second window. It left the answer
//! homeless: the ack was an ephemeral message nothing ever resolved, and the
//! turn's result arrived as a separate `channel.say` reply. FR-002 closes the
//! loop — the slash arm defers (kind 5), hands the interaction token to the
//! turn, and the turn edits that deferred message into the answer.
//!
//! FR-006 moved the ack and the token hand-off out of `agent.rs` and into
//! `interactions::handle_invoked_request`, which now serves the slash arm and
//! both right-click context menus. These guards follow the behaviour into its
//! new home, the same way `discord_followup_tap_tool_loop_test.rs` guards
//! #1852.

/// The slash arm must delegate to the shared invoked-request helper, and that
/// helper must defer, never acknowledge: `Acknowledge` (kind 6) is only valid
/// for component interactions, which is the original #27 bug.
#[test]
fn slash_arm_defers_instead_of_acknowledging() {
    let agent = include_str!("../channels/discord/agent.rs");
    let start = agent
        .find("if let Interaction::Command(command) = &interaction {")
        .expect("slash-command arm present");
    let rest = &agent[start..];
    let end = rest
        .find("// Modal submissions (#383)")
        .expect("modal arm terminates the slash arm");
    let arm = &rest[..end];
    assert!(
        arm.contains("handle_invoked_request("),
        "FR-006: the slash arm must delegate to the shared invoked-request path"
    );

    let interactions = include_str!("../channels/discord/interactions.rs");
    let helper_start = interactions
        .find("pub(crate) async fn handle_invoked_request(")
        .expect("the shared invoked-request helper must exist");
    let helper_rest = &interactions[helper_start..];
    let helper_end = helper_rest
        .find("pub(crate) async fn route_followup_turn(")
        .expect("route_followup_turn terminates the invoked-request helper");
    let helper = &helper_rest[..helper_end];
    assert!(
        helper.contains("CreateInteractionResponse::Defer("),
        "FR-002: the slash path must defer (kind 5) so the answer can replace it"
    );
    assert!(
        !helper.contains("CreateInteractionResponse::Acknowledge"),
        "FR-001: `Acknowledge` is invalid on a slash command — it is the #27 banner"
    );
}

/// The token must reach the turn, and only the invoked-request path may supply
/// one: a tapped component already resolved its interaction with `UpdateMessage`.
#[test]
fn only_the_slash_arm_supplies_an_interaction_token() {
    let interactions = include_str!("../channels/discord/interactions.rs");
    let start = interactions
        .find("pub(crate) async fn handle_invoked_request(")
        .expect("the shared invoked-request helper must exist");
    let rest = &interactions[start..];
    let end = rest
        .find("pub(crate) async fn route_followup_turn(")
        .expect("route_followup_turn terminates the invoked-request helper");
    let helper = &rest[..end];
    assert!(
        helper.contains("Some(command.token.clone())"),
        "FR-002: the invoked path must hand the turn the interaction token"
    );

    // The tap branch is terminated by the select-menu arm.
    let agent = include_str!("../channels/discord/agent.rs");
    let tap_start = agent
        .find("FOLLOWUP_PREFIX)")
        .expect("follow-up tap branch present");
    let tap_rest = &agent[tap_start..];
    let tap_end = tap_rest
        .find("// Select menu pick (#382)")
        .expect("select-menu branch terminates the tap branch");
    let tap = &tap_rest[..tap_end];

    // Inspect the call's ARGUMENT LIST, not the whole branch: the branch
    // legitimately contains `Some(` (e.g. `if let Some(..)`) and the word
    // "token" in the comment above the call. Scanning the branch for those
    // substrings is what made this guard panic while the wiring was correct.
    let call_start = tap
        .find("route_followup_turn(")
        .expect("tap arm calls the follow-up helper");
    let call = &tap[call_start..];
    let call_end = call.find(".await").expect("the tap call is awaited");
    let args = &call[..call_end];
    assert!(
        args.contains("None,"),
        "FR-002 is slash-only: the tap arm must pass None as the interaction token"
    );
    assert!(
        !args.contains("token"),
        "FR-002 is slash-only: the tap arm must not hand the turn an interaction token"
    );
}

/// Delivery must try the token edit FIRST and fall back to a plain message
/// when the 15-minute window has closed — never drop the answer (AC-005).
#[test]
fn token_delivery_precedes_the_plain_message_fallback() {
    let interactions = include_str!("../channels/discord/interactions.rs");
    let start = interactions
        .find("pub(crate) async fn route_followup_turn(")
        .expect("tap-turn helper present");
    let body = &interactions[start..];

    let edit = body
        .find("edit_original_interaction_response(")
        .expect("FR-002: the deferred ack must be edited with the answer");
    let fallback = body
        .find("writes::say(&http, channel, &payload")
        .expect("AC-005: a fallback plain message must exist for a dead token");
    assert!(
        edit < fallback,
        "the token edit must be attempted before falling back to a plain message"
    );
    assert!(
        body.contains("delivered_via_token"),
        "the delivery loop must track whether the token edit succeeded"
    );
    assert!(
        body.contains(r"\u{2026}"),
        "AC-005: a continuation marker must tag fallback and overflow chunks"
    );
}
