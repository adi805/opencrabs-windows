//! The answer a `suggest_options` surface halts on (Slack, #2029).
//!
//! Every narration line folds into the turn's step group as a `Note`. A turn
//! that halts on the options tool leaves its answer as the run of notes just
//! before that tool entry, and the buttons post while the answer is still
//! folded inside the group. [`take_trailing_answer`] lifts that run out, and
//! [`post_halted_answer`] posts it as its own message before the buttons,
//! the same rule the Discord fix uses (#2028).

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use slack_morphism::prelude::*;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::SlackState;
use super::final_body::folded_paragraphs;
use super::governor::GatedWrites;
use super::handler::sync_step_group;
use super::tool_group::GroupEntry;

/// The tool whose `SuggestedOptions` surface halts the turn.
pub(crate) const OPTIONS_TOOL: &str = "suggest_options";

/// Remove the answer a surface halted on from the group, and return it.
///
/// The answer is the run of notes directly before the options tool entry.
/// Any other tool entry in between means those notes belonged to an earlier
/// step. Without an options entry the trailing run of notes is taken.
pub(crate) fn take_trailing_answer(steps: &mut Vec<GroupEntry>) -> Option<String> {
    let end = steps
        .iter()
        .rposition(|e| matches!(e, GroupEntry::Tool { name, .. } if name == OPTIONS_TOOL))
        .unwrap_or(steps.len());
    let start = steps[..end]
        .iter()
        .rposition(|e| !matches!(e, GroupEntry::Note(_)))
        .map_or(0, |i| i + 1);
    let notes: Vec<String> = steps[start..end]
        .iter()
        .filter_map(|e| match e {
            GroupEntry::Note(text) if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
        .collect();
    if notes.is_empty() {
        return None;
    }
    steps.drain(start..end);
    Some(notes.join("\n\n"))
}

/// The paragraphs of answers already posted before the buttons. The final
/// body strips them, so a sign-off that repeats the held answer does not
/// post it twice.
pub(crate) fn held_paragraphs(held: &[String]) -> Vec<String> {
    if held.is_empty() {
        return Vec::new();
    }
    folded_paragraphs(Some(held.join("\n\n")))
}

/// Await the note tasks spawned so far. The held answer is only complete in
/// the group once they have all folded in.
pub(crate) async fn drain_note_tasks(handles: &Arc<StdMutex<Vec<JoinHandle<()>>>>) {
    let pending = {
        let mut g = handles.lock().expect("poisoned");
        std::mem::take(&mut *g)
    };
    for h in pending {
        if let Err(e) = h.await {
            tracing::warn!(error = %e, "Slack options host: narration task panicked");
        }
    }
}

/// Lift the held answer out of the step group, re-render the group without
/// it, and post it before the buttons. A turn with no held answer is a no-op.
pub(crate) async fn post_halted_answer<'a>(
    session: &SlackClientSession<'a, slack_morphism::hyper_tokio::SlackClientHyperHttpsConnector>,
    slack_state: &Arc<SlackState>,
    channel: SlackChannelId,
    thread_ts: Option<SlackTs>,
    group_ts: &Arc<Mutex<Option<SlackTs>>>,
    steps: &Arc<Mutex<Vec<GroupEntry>>>,
    posted: &Arc<Mutex<Vec<String>>>,
) {
    let answer = {
        let mut s = steps.lock().await;
        take_trailing_answer(&mut s)
    };
    let Some(answer) = answer else {
        return;
    };
    let entries = steps.lock().await.clone();
    sync_step_group(
        session,
        slack_state,
        channel.clone(),
        thread_ts.clone(),
        group_ts,
        entries,
    )
    .await;
    // The note already holds mrkdwn, so it posts as is. Converting again
    // would mangle the markup.
    let blocks = super::blocks::blocks_from_mrkdwn(&answer);
    let mut req = SlackApiChatPostMessageRequest::new(
        channel,
        SlackMessageContent::new()
            .with_text(answer.clone())
            .with_blocks(blocks),
    );
    if let Some(ts) = thread_ts {
        req = req.with_thread_ts(ts);
    }
    if let Err(e) = session.post(&req).await {
        tracing::error!("Slack: failed to post the answer held before the buttons: {e}");
    }
    posted.lock().await.push(answer);
}
