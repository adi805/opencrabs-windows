//! Keeping a channel's typing indicator alive while work is detached (#812).
//!
//! Every channel expires its indicator after a few seconds, so it has to be
//! re-sent on a tick, and every channel needs the same unusual tail: spawning
//! a detached command ENDS the turn, so the turn's own loop stops at exactly
//! the moment the user most needs to see that something is happening.
//!
//! Telegram and Discord had that tail written out twice, with the same
//! before-sleeping ordering and the same rationale in both comments. Only the
//! ping differs, so only the ping is a parameter.
//!
//! The loop counts BOTH detached-work registries now (#1985): background
//! shell tasks and working sub-agents. Shell tasks alone went silent the
//! moment a turn ended with only agents mid-work, which is the same
//! dead-chat bug #812 fixed, half-covered. A child parked at
//! `AwaitingInput` is waiting on a human, not on the machine, so it must
//! not hold the indicator (#1984): counting it as live is the stuck typing
//! clock, the tick would spin forever on a follow-up that never arrives.

use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::brain::agent::service::background_tasks::BackgroundTaskManager;
use crate::brain::tools::subagent::SubAgentManager;

/// Ping `send` every `tick` for as long as `session_id` has detached work in
/// either registry: background shell tasks (`background`) or working
/// sub-agents (`agents`) (#1985). A child parked at `AwaitingInput` is
/// excluded: the round ended, a person has to answer, and a typing indicator
/// held open on a human is the stuck clock #1984 reports.
///
/// Returns immediately when neither manager is wired, which is what a surface
/// without detached-work support needs. The first ping goes out BEFORE the
/// first sleep: the turn has just ended and its own last ping is already
/// expiring, so waiting a full tick would leave a visible dead gap at the
/// handover.
pub(crate) async fn tick_while_detached<F, Fut>(
    background: Option<Arc<BackgroundTaskManager>>,
    agents: Option<Arc<SubAgentManager>>,
    session_id: Uuid,
    tick: Duration,
    mut send: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    if background.is_none() && agents.is_none() {
        return;
    }
    loop {
        let bg = background
            .as_ref()
            .map(|m| m.running_for(session_id))
            .unwrap_or(0);
        let ag = agents
            .as_ref()
            .map(|m| m.alive_counts_for(session_id).0)
            .unwrap_or(0);
        if bg + ag == 0 {
            break;
        }
        send().await;
        tokio::time::sleep(tick).await;
    }
}
