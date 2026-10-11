//! Live turn for a background-task completion on Slack (#1986).
//!
//! A detached command finishing resumes its session. This runs that turn the
//! way a typed message runs: a tool-group flow message opens in the session's
//! thread, tool steps fold into it while the agent works, the answer posts
//! back into the same thread, and the group settles with its footer.

use super::SlackState;
use super::flow_progress::build_flow_progress;
use super::handler::{
    make_approval_callback, post_final_text, settle_step_group, spawn_flow_ticker, sync_step_group,
};
use super::tool_group::TurnOutcome;
use crate::brain::agent::AgentService;
use crate::utils::sanitize::{redact_secrets, strip_llm_artifacts};
use slack_morphism::prelude::*;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Map a failed resume turn to the outcome its flow message settles on.
///
/// Mirrors the follow-up tap: a deadline or timeout reads as timed out,
/// anything else as failed.
pub(crate) fn resume_failure_outcome(message: &str) -> TurnOutcome {
    let lower = message.to_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") || lower.contains("deadline") {
        TurnOutcome::TimedOut
    } else {
        TurnOutcome::Failed
    }
}

/// Run one resumed turn and deliver its answer into the session's thread.
pub(crate) async fn run_resume_flow_turn(
    state: Arc<SlackState>,
    client: Arc<SlackHyperClient>,
    agent: Arc<AgentService>,
    bot_token: String,
    session_id: Uuid,
    channel_id: String,
    context_text: String,
) {
    let channel = SlackChannelId::new(channel_id.clone());
    let thread = state.session_thread(session_id).await.map(SlackTs::new);
    let token = SlackApiToken::new(SlackApiTokenValue::from(bot_token.clone()));
    let session = client.open_session(&token);

    // Open the flow group before the agent starts, so the live clock covers
    // the thinking latency as it does for a typed message.
    let turn_group_ts: Arc<Mutex<Option<SlackTs>>> = Arc::new(Mutex::new(None));
    sync_step_group(
        &session,
        &state,
        channel.clone(),
        thread.clone(),
        &turn_group_ts,
        Vec::new(),
    )
    .await;
    spawn_flow_ticker(client.clone(), state.clone(), turn_group_ts.clone());

    let flow = build_flow_progress(
        client.clone(),
        state.clone(),
        channel.clone(),
        thread.clone(),
        turn_group_ts.clone(),
        bot_token,
    );
    let approval_cb = make_approval_callback(state.clone());

    let result = agent
        .send_push_turn(
            session_id,
            context_text,
            None,
            None,
            Some(approval_cb),
            Some(flow.cb),
            "slack",
            Some(&channel_id),
            None,
        )
        .await;

    // Drain narration tasks so the group is settled from its full contents.
    let pending = {
        let mut g = flow.intermediate_handles.lock().expect("poisoned");
        std::mem::take(&mut *g)
    };
    for h in pending {
        if let Err(e) = h.await {
            tracing::warn!(error = %e, "Slack resume: intermediate fold task panicked");
        }
    }

    match result {
        Ok(response) => {
            let answer = strip_llm_artifacts(&response.content);
            let (answer, _img) = crate::utils::extract_img_markers(&answer);
            let (answer, _vid) = crate::utils::extract_vid_markers(&answer);
            let answer = redact_secrets(&answer);
            if !answer.trim().is_empty() {
                for chunk in super::handler::split_message(&answer, 3000) {
                    if chunk.trim().is_empty() {
                        continue;
                    }
                    post_final_text(&session, &channel_id, thread.as_ref(), chunk).await;
                }
            }
            let ctx_max = agent.context_limit_for_session(session_id);
            let footer = crate::utils::format_ctx_footer(
                response.context_tokens,
                ctx_max,
                response.tokens_per_second,
            );
            settle_step_group(
                &session,
                &state,
                agent.as_ref(),
                session_id,
                channel.clone(),
                &channel_id,
                &turn_group_ts,
                TurnOutcome::Finished,
                &footer,
            )
            .await;
        }
        Err(e) => {
            tracing::warn!("[bg-resume] slack: resume turn failed for session {session_id}: {e}");
            let outcome = resume_failure_outcome(&e.to_string());
            settle_step_group(
                &session,
                &state,
                agent.as_ref(),
                session_id,
                channel.clone(),
                &channel_id,
                &turn_group_ts,
                outcome,
                "",
            )
            .await;
        }
    }
}
