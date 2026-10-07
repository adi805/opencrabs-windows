//! #1989: WhatsApp's composing indicator had three gaps: the post-turn tail
//! duplicated the shared tick loop (so `/stop` could not end it and a new
//! turn stacked a second loop under the first), counted only background
//! shell tasks (so sub-agent work left the chat silent), and the resume turn
//! ran with no indicator at all. The loops live inside spawned handlers
//! holding live network clients, so these pins are source-level (the same
//! approach as the Slack thread persistence test); the behavioral halves
//! run against the shared helper directly in `channels_typing_tick_test.rs`.

const HANDLER: &str = include_str!("../channels/whatsapp/handler.rs");
const RESUME: &str = include_str!("../channels/whatsapp/resume.rs");

#[test]
fn tail_runs_through_the_shared_tick_not_a_copy() {
    assert!(
        HANDLER.contains("crate::channels::typing_tick::tick_while_detached("),
        "the handover tail must call the shared tick, not re-roll its own loop"
    );
    assert_eq!(
        HANDLER.matches("manager.running_for(").count(),
        0,
        "the duplicated `while manager.running_for` tail is the bug (#1989); \
         the shared helper owns that predicate now"
    );
}

#[test]
fn tail_counts_subagents_and_watches_the_stop_token() {
    assert!(
        HANDLER.contains("let agents = agent.subagent_manager();"),
        "the typing task must grab the sub-agent registry: shell-task-only \
         counting went silent the moment a turn ended with working agents"
    );
    assert!(
        HANDLER.contains("Some(stop.clone()),"),
        "the tail passes the session stop token into the shared tick, so \
         /stop and the next turn's registration can end the indicator"
    );
    assert!(
        HANDLER.contains("if !stop.is_cancelled()"),
        "paused is only sent after the work ends on its own; a /stop or a \
         new turn owns what the user sees from there"
    );
}

#[test]
fn stop_token_is_born_before_the_task_that_watches_it() {
    let created = HANDLER
        .find("let cancel_token = CancellationToken::new();")
        .expect("the session cancel token must be created in the handler");
    let watched = HANDLER
        .find("let stop = cancel_token.clone();")
        .expect("the typing task must clone the session token as its stop");
    let stored = HANDLER
        .find(".store_cancel_token(session_id, cancel_token.clone())")
        .expect("the token must still be registered for /stop");
    assert!(
        created < watched && watched < stored,
        "order must be create -> typing task takes its clone -> register: \
         created {created}, watched {watched}, stored {stored}"
    );
    assert_eq!(
        HANDLER
            .matches("let cancel_token = CancellationToken::new();")
            .count(),
        1,
        "exactly one token per turn; two would split /stop from the tail"
    );
}

#[test]
fn resume_turn_keeps_composing_until_the_delivery_path_ends() {
    assert!(
        RESUME.contains("let typing = tokio::spawn("),
        "the resume turn must run a composing ticker across the slowest \
         message of the cycle (the tail loop had already stopped)"
    );
    assert!(
        RESUME.contains("typing.abort();"),
        "the ticker must be cut when the flow ends, on every exit path"
    );
    // The abort sits after the wrapped flow, not inside an early-return arm:
    // a `return` from `resume_flow` still lands on the abort line.
    let flow = RESUME
        .find("let resume_flow = async {")
        .expect("the resume body must be wrapped so one cancel covers it all");
    let abort = RESUME.find("typing.abort();").expect("abort present");
    assert!(
        flow < abort,
        "the flow must open before the abort that ends its ticker"
    );
    assert!(
        RESUME.contains("async fn send_composing_now"),
        "the ticker's ping needs the transport-aware helper: state.client() \
         is async and may be None while the socket reconnects"
    );
}
