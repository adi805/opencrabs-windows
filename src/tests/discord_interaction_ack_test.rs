//! #1888: the application-command path must ack invocations with the
//! command-correct deferred callback, not the component-only `Acknowledge`.
//!
//! `CreateInteractionResponse::Acknowledge` serializes to Discord callback
//! type 6 (DEFERRED_UPDATE_MESSAGE), documented in serenity as "Only valid
//! for component-based interactions". An application command acked with it
//! is never marked resolved, so Discord shows "The application didn't
//! respond in time" over the invocation line even when the answer landed in
//! the channel normally. Type 5 (DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE),
//! i.e. `CreateInteractionResponse::Defer`, is the deferred ack for
//! commands.
//!
//! FR-006 moved the ack out of `agent.rs` and into the shared
//! `interactions::handle_invoked_request`, which now serves the slash arm and
//! both right-click context menus. The `agent.rs` arm is wiring only, so the
//! #1888 pins moved with the behaviour: the arm must delegate, and the helper
//! must defer.
//!
//! This is a source-scan pin (same established pattern as
//! `clear_context_test.rs` and `discord_flow_ticker_test.rs` against
//! handler.rs), scoped to the invoked-request path ONLY: the component and
//! modal paths legitimately keep `Acknowledge`.

/// The `Interaction::Command` arm in agent.rs, which is now wiring only.
fn command_arm() -> &'static str {
    let src = include_str!("../channels/discord/agent.rs");
    let start = src
        .find("if let Interaction::Command(command)")
        .expect("the Interaction::Command branch must exist in agent.rs");
    let end = src[start..]
        .find("if let Interaction::Modal(modal)")
        .expect("the Interaction::Modal branch must exist after the Command branch");
    &src[start..start + end]
}

/// The shared ack + dispatch helper the arm delegates to.
fn invoked_request_body() -> &'static str {
    let src = include_str!("../channels/discord/interactions.rs");
    let start = src
        .find("pub(crate) async fn handle_invoked_request(")
        .expect("the shared invoked-request helper must exist in interactions.rs");
    let rest = &src[start..];
    let end = rest
        .find("pub(crate) async fn route_followup_turn(")
        .expect("route_followup_turn terminates the invoked-request helper");
    &rest[..end]
}

#[test]
fn command_branch_delegates_to_the_shared_invoked_request_path() {
    let arm = command_arm();
    assert!(
        arm.contains("handle_invoked_request("),
        "FR-006: the command arm must hand the request to the shared helper, \
         so the slash arm and the context menus share one ack and one gate"
    );
    assert!(
        !arm.contains("CreateInteractionResponse::Defer"),
        "the ack belongs in the shared helper, not duplicated in the arm"
    );
}

#[test]
fn command_branch_acks_with_deferred_channel_message_source() {
    let body = invoked_request_body();
    assert!(
        body.contains("CreateInteractionResponse::Defer"),
        "#1888: the invoked-request path must ack with CreateInteractionResponse::Defer \
         (callback type 5, DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE)"
    );
    assert!(
        !body.contains("CreateInteractionResponse::Acknowledge"),
        "#1888: the invoked-request path must NOT ack with CreateInteractionResponse::Acknowledge \
         (callback type 6 is component-only and never resolves an invocation)"
    );
}

#[test]
fn command_branch_ack_result_is_inspected() {
    // The old bug swallowed the ack result (`let _ack = ...`), so a refused
    // acknowledgement was invisible. The path must bind the result and log a
    // failure instead of dropping it.
    let body = invoked_request_body();
    assert!(
        !body.contains("let _ack"),
        "#1888: the command ack result must not be discarded with `let _ack`"
    );
    assert!(
        body.contains("tracing::warn!") && body.contains("refused"),
        "#1888: a refused command ack must be logged at warn level"
    );
}
