//! Network half of the WhatsApp live tool block (#2023).
//!
//! One living message per background-completion turn. It opens before the
//! agent runs so the clock covers the thinking, re-renders on a tick, and
//! settles once with the turn's outcome. Progress arrives on a channel and a
//! single task owns every send and edit, the same ordering rule `stream.rs`
//! follows.
//!
//! Failure degrades, never drops. A rejected edit (the 15-minute window
//! closed, or the server refused it) stops the running updates instead of
//! posting a new block on every tick. The settled state still lands once.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wacore_binary::jid::Jid;
use whatsapp_rust::client::Client;

use super::WhatsAppState;
use super::flow_render::{Phase, Step, fold, render};
use crate::brain::agent::{ProgressCallback, ProgressEvent};

/// How often the running block re-renders, so the clock moves.
const TICK: Duration = Duration::from_secs(5);

/// Everything the block needs that is not the channel itself.
pub(crate) struct FlowConfig {
    pub client: Arc<Client>,
    pub jid: Jid,
    pub state: Arc<WhatsAppState>,
    pub rate_limit: crate::config::WaRateLimitConfig,
    pub is_owner: bool,
}

enum FlowMsg {
    Progress(ProgressEvent),
    Settle(Phase),
}

/// Handle the turn writes into. Cloneable through its callback.
pub(crate) struct FlowSender {
    tx: mpsc::UnboundedSender<FlowMsg>,
}

impl FlowSender {
    /// Progress callback for `send_push_turn`.
    pub(crate) fn progress_callback(&self) -> ProgressCallback {
        let tx = self.tx.clone();
        Arc::new(move |_session_id, event| {
            if let Err(e) = tx.send(FlowMsg::Progress(event)) {
                tracing::warn!(
                    target: "whatsapp",
                    error = %e,
                    "flow event arrived after the block settled; not shown"
                );
            }
        })
    }

    /// Settle the block with the turn's outcome. Call once, after the turn.
    pub(crate) fn settle(&self, phase: Phase) {
        if let Err(e) = self.tx.send(FlowMsg::Settle(phase)) {
            tracing::warn!(
                target: "whatsapp",
                error = %e,
                "flow settle arrived after the block task exited"
            );
        }
    }
}

/// Start the task that owns the living block for one turn.
pub(crate) fn spawn(config: FlowConfig) -> (FlowSender, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<FlowMsg>();
    let handle = tokio::spawn(async move {
        let mut block = Block::new(config);
        block.flush(Phase::Running).await;
        let mut tick = tokio::time::interval(TICK);
        // The first tick fires at once; the flush above already covered it.
        tick.tick().await;
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(FlowMsg::Progress(event)) => {
                        block.apply(&event);
                    }
                    Some(FlowMsg::Settle(phase)) => {
                        block.flush(phase).await;
                        break;
                    }
                    None => {
                        // Every sender dropped without a settle: the turn
                        // ended some other way. Show it as failed, not running.
                        tracing::warn!(
                            target: "whatsapp",
                            "flow block closed without settling; marking it failed"
                        );
                        block.flush(Phase::Failed).await;
                        break;
                    }
                },
                _ = tick.tick() => {
                    block.flush(Phase::Running).await;
                }
            }
        }
    });
    (FlowSender { tx }, handle)
}

/// The block's state, owned by the single task.
struct Block {
    config: FlowConfig,
    steps: Vec<Step>,
    started: Instant,
    /// Server id of the posted block, once it exists.
    message_id: Option<String>,
    /// Text the posted block currently shows, to skip no-op edits.
    last_text: Option<String>,
    /// Set when the block can no longer be edited. Running updates stop;
    /// the settled state gets one fresh post.
    expired: bool,
}

impl Block {
    fn new(config: FlowConfig) -> Self {
        Self {
            config,
            steps: Vec::new(),
            started: Instant::now(),
            message_id: None,
            last_text: None,
            expired: false,
        }
    }

    fn apply(&mut self, event: &ProgressEvent) {
        fold(&mut self.steps, event);
    }

    async fn flush(&mut self, phase: Phase) {
        let settled = phase != Phase::Running;
        if self.expired && !settled {
            return;
        }
        let text = render(&self.steps, phase, self.started.elapsed());
        if self.last_text.as_deref() == Some(text.as_str()) {
            return;
        }

        match self.message_id.clone() {
            Some(id) if !self.expired => {
                if !settled && !self.gate().await {
                    return;
                }
                let message = waproto::whatsapp::Message {
                    conversation: Some(text.clone()),
                    ..Default::default()
                };
                match self
                    .config
                    .client
                    .edit_message(self.config.jid.clone(), id, message)
                    .await
                {
                    Ok(_) => self.last_text = Some(text),
                    Err(e) => {
                        tracing::warn!(
                            target: "whatsapp",
                            error = %e,
                            "flow block edit rejected; running updates stop for this turn"
                        );
                        self.expired = true;
                        if settled {
                            self.post(text, settled).await;
                        }
                    }
                }
            }
            _ => self.post(text, settled).await,
        }
    }

    /// Post a new block and track it as the one future edits target.
    async fn post(&mut self, text: String, settled: bool) {
        if !settled && !self.gate().await {
            return;
        }
        let message = waproto::whatsapp::Message {
            conversation: Some(text.clone()),
            ..Default::default()
        };
        match self
            .config
            .client
            .send_message(self.config.jid.clone(), message)
            .await
        {
            Ok(result) => {
                self.message_id = Some(result.message_id);
                self.last_text = Some(text);
                self.expired = false;
            }
            Err(e) => {
                tracing::warn!(
                    target: "whatsapp",
                    error = %e,
                    "flow block post failed; the turn continues without a live block"
                );
                self.expired = true;
            }
        }
    }

    /// Running updates are one stanza each, so they pace on the bucket and
    /// drop under a saturated daily cap (#1407). The settled state is never
    /// gated: it is the one update that must reach the user.
    async fn gate(&self) -> bool {
        let allowed = self
            .config
            .state
            .rate_limiter
            .gate_ephemeral(&self.config.rate_limit, self.config.is_owner)
            .await;
        if !allowed {
            tracing::debug!(
                target: "whatsapp",
                "daily cap reached; skipping a live tool block update"
            );
        }
        allowed
    }
}
