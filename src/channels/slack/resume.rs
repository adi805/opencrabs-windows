//! Background-task resume producer for Slack (#731).
//!
//! Mirrors Telegram's `build_enqueue_callback`: when a detached long command
//! finishes, resume the originating session in its Slack channel. The turn
//! runs as a live flow message (#1986) and the answer lands in the session's
//! thread, the same shape a typed message gets.

use super::SlackState;
use crate::brain::agent::service::MessageEnqueueCallback;
use crate::channels::bg_resume::{self, AgentHolder};
use std::sync::Arc;

pub(crate) fn build_enqueue_callback(
    state: Arc<SlackState>,
    agent_holder: AgentHolder,
) -> MessageEnqueueCallback {
    Arc::new(move |session_id, msg| {
        let state = state.clone();
        let agent_holder = agent_holder.clone();
        tokio::spawn(async move {
            let Some(channel) = state.session_channel(session_id).await else {
                tracing::warn!("[bg-resume] slack: no channel for session {session_id}; dropping");
                return;
            };
            // Transport first: the flow message posts live through the client,
            // so the bounded wait (#1242) runs before the turn, as on Discord.
            let Some((token_val, client)) =
                crate::channels::transport_ready::await_transport("slack", session_id, || async {
                    match (state.bot_token().await, state.client().await) {
                        (Some(token), Some(client)) => Some((token, client)),
                        _ => None,
                    }
                })
                .await
            else {
                return;
            };
            let Some(agent) = bg_resume::upgrade(&agent_holder) else {
                tracing::warn!("[bg-resume] slack: agent gone; dropping resume");
                return;
            };
            super::resume_turn::run_resume_flow_turn(
                state.clone(),
                client.clone(),
                agent.clone(),
                token_val.clone(),
                session_id,
                channel.clone(),
                msg.context_text,
            )
            .await;
            // The completion just delivered is the event the channel's waiting
            // step group is holding for (#1988): flip it now instead of
            // leaving the ⏳ line until the next inbound message.
            super::handler::flip_waiting_group(
                &client,
                &state,
                agent.as_ref(),
                token_val,
                &channel,
            )
            .await;
        });
    })
}
