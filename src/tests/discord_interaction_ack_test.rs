//! #1888: the application-command branch of the Discord interaction
//! handler must ack invocations with the command-correct deferred
//! callback, not the component-only `Acknowledge`.
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
//! This is a source-scan pin (same established pattern as
//! `clear_context_test.rs` and `discord_flow_ticker_test.rs` against
//! handler.rs), scoped to the `Interaction::Command` branch ONLY: the
//! component and modal paths legitimately keep `Acknowledge`.

fn command_branch() -> &'static str {
    let src = include_str!("../channels/discord/agent.rs");
    let start = src
        .find("if let Interaction::Command(command)")
        .expect("the Interaction::Command branch must exist in agent.rs");
    let end = src[start..]
        .find("if let Interaction::Modal(modal)")
        .expect("the Interaction::Modal branch must exist after the Command branch");
    &src[start..start + end]
}

#[test]
fn command_branch_acks_with_deferred_channel_message_source() {
    let branch = command_branch();
    assert!(
        branch.contains("CreateInteractionResponse::Defer"),
        "#1888: the command branch must ack with CreateInteractionResponse::Defer \
         (callback type 5, DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE)"
    );
    assert!(
        !branch.contains("CreateInteractionResponse::Acknowledge"),
        "#1888: the command branch must NOT ack with CreateInteractionResponse::Acknowledge \
         (callback type 6 is component-only and never resolves an invocation)"
    );
}

#[test]
fn command_branch_ack_result_is_inspected() {
    // The old bug swallowed the ack result (`let _ack = ...`), so a refused
    // acknowledgement was invisible. The branch must bind the result and
    // log a failure instead of dropping it.
    let branch = command_branch();
    assert!(
        !branch.contains("let _ack"),
        "#1888: the command ack result must not be discarded with `let _ack`"
    );
    assert!(
        branch.contains("tracing::warn!") && branch.contains("refused"),
        "#1888: a refused command ack must be logged at warn level"
    );
}
