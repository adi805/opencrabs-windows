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

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::brain::agent::service::background_tasks::BackgroundTaskManager;
use crate::brain::tools::subagent::SubAgentManager;

/// Default ceiling for every handover tail (#1984): 30 minutes, matching the
/// flow ticker. A tail is a handover, not a lease on the indicator: whatever
/// is counted, the loop still ends at some point, so even a row that leaks
/// through every cleanup above cannot pin a chat's typing forever.
pub(crate) const DEFAULT_TICK_CEILING: Duration = Duration::from_secs(30 * 60);

/// Ping `send` every `tick` for as long as `session_id` has detached work in
/// either registry: background shell tasks (`background`) or working
/// sub-agents (`agents`) (#1985). A child parked at `AwaitingInput` is
/// excluded: the round ended, a person has to answer, and a typing indicator
/// held open on a human is the stuck clock #1984 reports.
///
/// `stop` is an optional hard end for the indicator itself (#1989): WhatsApp
/// reuses this loop for its post-turn tail, and `/stop` or the next turn's
/// registration must end it rather than leave a second loop pinging under
/// the new turn's own indicator. Checked before every ping and raced against
/// the sleep, so a cancelled token ends the loop within one tick, and a token
/// that was already cancelled ends it before the first ping.
///
/// `ceiling` is the hard bound on the loop's total life (#1984), counted
/// from the handover, checked before every ping. It is the last line of
/// defense: stuck-entry sources are fixed at the root (mirror rows clear on
/// any terminal notification and on CLI exit; `run_detached` waits the shell
/// under a ceiling), and this ensures a future leak of any row kind pings at
/// most `ceiling` and not forever.
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
    stop: Option<CancellationToken>,
    tick: Duration,
    ceiling: Duration,
    mut send: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    if background.is_none() && agents.is_none() {
        return;
    }
    let born = std::time::Instant::now();
    loop {
        if stop.as_ref().is_some_and(CancellationToken::is_cancelled) {
            break;
        }
        if born.elapsed() >= ceiling {
            break;
        }
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
        match stop.as_ref() {
            Some(token) => tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(tick) => {}
            },
            None => tokio::time::sleep(tick).await,
        }
    }
}
