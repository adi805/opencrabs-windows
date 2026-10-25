//! `/stop --session <id>` (#2108): targeted cross-session cancel.
//!
//! Plain `/stop` only reaches the session bound to the chat the command
//! arrived in, so an operator watching a stuck turn in another topic had no
//! way to cancel it. These tests pin the parse arm: owner gate, the full-id
//! path (which must not depend on the recent-sessions window), the prefix
//! path, and the refusals that keep a typo from degrading into a plain
//! `/stop` on the wrong session.

use crate::channels::commands::{ChannelCommand, handle_command};
use crate::tests::agent_service_mocks::create_test_service_full;
use uuid::Uuid;

#[tokio::test]
async fn owner_can_target_a_session_by_full_id() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command(
        &format!("/stop --session {sid}"),
        sid,
        &agent,
        &svc,
        true,
        None,
    )
    .await;
    match cmd {
        ChannelCommand::StopSession { session_id, label } => {
            assert_eq!(session_id, sid, "the target must be the id that was typed");
            assert_eq!(label, "Test Session");
        }
        _ => panic!("a full session id must resolve to StopSession"),
    }
}

#[tokio::test]
async fn the_short_flag_is_accepted_too() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command(&format!("/stop -s {sid}"), sid, &agent, &svc, true, None).await;
    assert!(
        matches!(cmd, ChannelCommand::StopSession { session_id, .. } if session_id == sid),
        "-s must be an alias for --session"
    );
}

#[tokio::test]
async fn plain_stop_still_means_this_session() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/stop", sid, &agent, &svc, true, None).await;
    assert!(
        matches!(cmd, ChannelCommand::Stop),
        "no argument must keep the original single-session behaviour"
    );
}

#[tokio::test]
async fn a_prefix_resolves_against_the_session_list() {
    let (agent, svc, sid) = create_test_service_full().await;
    let prefix: String = sid.to_string().chars().take(8).collect();
    let cmd = handle_command(
        &format!("/stop --session {prefix}"),
        sid,
        &agent,
        &svc,
        true,
        None,
    )
    .await;
    assert!(
        matches!(cmd, ChannelCommand::StopSession { session_id, .. } if session_id == sid),
        "an 8-character prefix of the only session must resolve to it"
    );
}

#[tokio::test]
async fn an_unknown_prefix_is_refused_with_the_usage_line() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/stop --session deadbeef", sid, &agent, &svc, true, None).await;
    match cmd {
        ChannelCommand::UnknownCommand(msg) => {
            assert!(msg.contains("No session matches"), "{msg}");
            assert!(
                msg.contains("/sessions"),
                "must point at the id list: {msg}"
            );
        }
        _ => panic!("a prefix matching nothing must be refused, not sent to the agent"),
    }
}

#[tokio::test]
async fn a_title_is_refused_rather_than_guessed() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command(
        "/stop --session Test Session",
        sid,
        &agent,
        &svc,
        true,
        None,
    )
    .await;
    match cmd {
        ChannelCommand::UnknownCommand(msg) => {
            assert!(msg.contains("is not a session id"), "{msg}");
        }
        _ => panic!("a title must be refused: resolving it by name is guesswork"),
    }
}

#[tokio::test]
async fn an_unknown_flag_does_not_fall_back_to_plain_stop() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/stop --id deadbeef", sid, &agent, &svc, true, None).await;
    match cmd {
        ChannelCommand::UnknownCommand(msg) => {
            assert!(msg.contains("Unknown option"), "{msg}");
        }
        _ => panic!("an unknown flag must be refused, never treated as bare /stop"),
    }
}

#[tokio::test]
async fn a_missing_value_is_refused() {
    let (agent, svc, sid) = create_test_service_full().await;
    let cmd = handle_command("/stop --session", sid, &agent, &svc, true, None).await;
    match cmd {
        ChannelCommand::UnknownCommand(msg) => {
            assert!(msg.contains("Missing a session id"), "{msg}")
        }
        _ => panic!("--session with no value must be refused"),
    }
}

#[tokio::test]
async fn a_targeted_stop_is_owner_only() {
    let (agent, svc, sid) = create_test_service_full().await;
    let other = Uuid::new_v4();
    let cmd = handle_command(
        &format!("/stop --session {other}"),
        sid,
        &agent,
        &svc,
        false,
        None,
    )
    .await;
    match cmd {
        ChannelCommand::UnknownCommand(msg) => {
            assert!(msg.to_lowercase().contains("owner"), "{msg}");
        }
        _ => panic!("a non-owner must not be able to cancel another session's turn"),
    }
}
