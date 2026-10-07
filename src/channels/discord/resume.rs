//! Background-task resume producer for Discord (#731).
//!
//! Mirrors Telegram's `build_enqueue_callback`: when a detached long command
//! finishes, resume the originating session and deliver the result to its
//! Discord channel. Discord has no streaming resume pipeline, so this sends the
//! completed turn's final text (like the crash-recovery path in `cli/ui.rs`).

use super::DiscordState;
use crate::brain::agent::service::MessageEnqueueCallback;
use crate::channels::background_work::{bg_indicator_for, subagent_counts_for, waiting_verb};
use crate::channels::bg_resume::{self, AgentHolder};
use std::sync::Arc;

/// Discord rejects any message over 2000 characters with `Message too large`
/// and drops the entire payload (#1899), so a long resumed verdict that fails
/// never reaches the channel at all. Chunk it through the same fence- and
/// markup-aware splitter the live delivery path uses
/// (`handler::split_message`, #876).
///
/// Blank content yields no chunks: an empty `say` is a 400 and the splitter
/// would hand back one empty chunk for it.
pub(crate) fn resume_delivery_chunks(content: &str) -> Vec<String> {
    if content.trim().is_empty() {
        return Vec::new();
    }
    super::handler::split_message(content, 2000)
}

pub(crate) fn build_enqueue_callback(
    state: Arc<DiscordState>,
    agent_holder: AgentHolder,
) -> MessageEnqueueCallback {
    Arc::new(move |session_id, msg| {
        let state = state.clone();
        let agent_holder = agent_holder.clone();
        tokio::spawn(async move {
            let Some(channel_id) = state.session_channel(session_id).await else {
                tracing::warn!(
                    "[bg-resume] discord: no channel for session {session_id}; dropping"
                );
                return;
            };
            // Bounded wait rather than a drop (#1242): at boot the transport
            // is usually seconds away, and returning here lost the wake for
            // good.
            let Some(http) =
                crate::channels::transport_ready::await_transport("discord", session_id, || {
                    state.http()
                })
                .await
            else {
                return;
            };
            let Some(agent) = bg_resume::upgrade(&agent_holder) else {
                tracing::warn!("[bg-resume] discord: agent gone; dropping resume");
                return;
            };
            // #1987: a completion just landed. If this session's turn settled
            // to a ⏳ waiting line, re-fold the verb from both registries and
            // re-render: the line narrows as work drains and flips to the
            // plain finished check when it empties. Sessions with no waiting
            // group are untouched; an aged-out group drops its registration.
            if let Some(gmid) = state.waiting_group_for(session_id).await {
                let (_, bg_count) = bg_indicator_for(&agent, session_id);
                let verb = waiting_verb(bg_count, subagent_counts_for(&agent, session_id));
                if let Some(group) = state.refresh_waiting_line(gmid, verb).await {
                    let edit = serenity::builder::EditMessage::new()
                        .content(super::tool_group::render_content(&group))
                        .components(super::tool_group::render_components(&group, gmid));
                    let flip_mid = serenity::model::id::MessageId::new(gmid);
                    if let Err(e) = serenity::model::id::ChannelId::new(channel_id)
                        .edit_message(&http, flip_mid, edit)
                        .await
                    {
                        tracing::debug!("[bg-resume] discord: waiting-line flip edit failed: {e}");
                    }
                } else {
                    state.clear_waiting_group(session_id).await;
                }
            }
            let target = channel_id.to_string();
            if let Some(content) =
                bg_resume::run_resume_turn(agent, session_id, msg.context_text, "discord", &target)
                    .await
            {
                let chunks = resume_delivery_chunks(&content);
                let total = chunks.len();
                if total == 0 {
                    tracing::debug!("[bg-resume] discord: blank verdict, nothing sent");
                    return;
                }
                let verdict_chars = content.chars().count();
                let ch = serenity::model::id::ChannelId::new(channel_id);
                for (index, chunk) in chunks.into_iter().enumerate() {
                    if let Err(e) = ch.say(&http, &chunk).await {
                        if index == 0 {
                            tracing::error!(
                                "[bg-resume] discord: first of {total} chunks failed, the whole verdict was lost ({verdict_chars} chars): {e}"
                            );
                        } else {
                            tracing::warn!(
                                "[bg-resume] discord: chunk {}/{total} failed, the rest of the verdict was dropped: {e}",
                                index + 1
                            );
                        }
                        break;
                    }
                }
            }
        });
    })
}
