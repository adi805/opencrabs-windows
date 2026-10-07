//! The shared [`DiscordState`] struct: fields, `new()` and `Default`.
//!
//! Every field is `pub(super)` so the per-concern impl modules beside this
//! file (`approval`, `cancel`, `connection`, `pending_interactions`,
//! `sessions`, `tool_group`) can reach them without widening the
//! crate-visible surface. Behaviour lives there, not here.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{interactions, long_answer, tool_group};

/// Shared Discord state for proactive messaging.
///
/// Set when the bot connects via the `ready` event.
/// Read by the `discord_send` tool to send messages on demand.
pub struct DiscordState {
    pub(super) http: Mutex<Option<Arc<serenity::http::Http>>>,
    /// Channel ID of the owner's last message — used as default for proactive sends
    pub(super) owner_channel_id: Mutex<Option<u64>>,
    /// Bot's own user ID — set on ready, used for @mention detection
    pub(super) bot_user_id: Mutex<Option<u64>>,
    /// Guild ID of the last guild message — needed for guild-scoped actions
    pub(super) guild_id: Mutex<Option<u64>>,
    /// Comparison key of the application command set currently registered with
    /// Discord (the command set plus guild membership), which also tells us
    /// whether the watcher that keeps it in sync has been started: `ready`
    /// fires on every reconnect and the watcher must not be started twice.
    /// `None` until a sync succeeds, and set back to `None` when every guild
    /// refused, so a failed attempt is retried rather than remembered as done.
    /// See `commands::sync_commands`.
    pub(super) commands_sig: Mutex<Option<u64>>,
    /// Whether the config watcher that keeps [`Self::commands_sig`] in sync has
    /// been started. Separate from the key itself because a failed sync leaves
    /// the key `None`, and `ready` would otherwise spawn a new watcher on every
    /// reconnect.
    pub(super) commands_watcher_started: Mutex<bool>,
    /// Maps session_id → channel_id for approval routing
    pub(super) session_channels: Mutex<HashMap<Uuid, u64>>,
    /// Reverse ownership map (#148): channel_id → session_id, written in
    /// lockstep with `session_channels` at `register_session_channel` (the
    /// ONLY write site for both). Last writer wins — mirrors the forward map.
    pub(super) channel_sessions: Mutex<HashMap<u64, Uuid>>,
    /// Pending approval channels: approval_id → oneshot sender of (approved, always)
    pub(super) pending_approvals: Mutex<HashMap<String, oneshot::Sender<(bool, bool)>>>,
    /// Per-session cancel tokens for aborting in-flight agent tasks via /stop
    pub(super) cancel_tokens: Mutex<HashMap<Uuid, CancellationToken>>,
    /// Pending select menus: id -> (created, options) (#382). Lazy TTL:
    /// stale picks answer "expired" (#386).
    pub(super) pending_selects: Mutex<HashMap<String, (std::time::Instant, Vec<String>)>>,
    /// Pending modal forms: id -> (created, spec) (#383). Same lazy TTL.
    pub(super) pending_forms: Mutex<HashMap<String, (std::time::Instant, interactions::FormSpec)>>,
    /// Long answers paged behind a button (FR-009), keyed by the id of the
    /// message carrying page 0. Insertion-ordered for pruning; bounded at
    /// `DiscordState::LONG_ANSWER_CAP` (see `long_answer`).
    pub(super) long_answers: Mutex<long_answer::LongAnswerStore>,
    /// Collapsible tool groups keyed by message id, so the Expand/Collapse
    /// interaction can re-render after the turn ended. Insertion-ordered
    /// for pruning; bounded at [`Self::TOOL_GROUP_CAP`] (see `tool_group`).
    pub(super) tool_groups: Mutex<(Vec<u64>, HashMap<u64, tool_group::GroupState>)>,
    /// Plan cards: session_id → (channel_id, message_id, signature).
    ///
    /// Process-local cache; [`Self::plan_card_store`] is the durable backing
    /// and the same rows Telegram reads and writes (FR-008 / AC-019: no third
    /// state mechanism). The signature is the rendered body plus its keyboard
    /// state, so an unchanged plan costs no API call.
    pub(super) plan_cards: Mutex<HashMap<Uuid, (u64, u64, String)>>,
    /// Durable backing for [`Self::plan_cards`] (#104). The map alone is
    /// process-local, so a restart lost which message carried the card — it
    /// could then be neither edited (no tracked id) nor removed, stranding a
    /// stale checklist in the channel. Same table and repository Telegram uses
    /// (#809); wired at startup next to `DiscordState::new()`.
    pub(super) plan_card_store: Mutex<Option<crate::db::repository::PlanCardRepository>>,
    /// Per-session lock serialising plan-card writes, mirroring Telegram's
    /// `plan_card_locks` (#822). Without it two concurrent refreshes both
    /// see no card, both post one, and the second id overwrites the first —
    /// leaving a card visible but untracked, so it can never be edited or
    /// removed again.
    ///
    /// Rate-limit backoff is deliberately NOT duplicated here: the Discord
    /// governor (FR-003) already owns every write and applies its own
    /// per-class bucket, which is the same reuse-over-reinvent rule AC-019
    /// states for the lock.
    pub(super) plan_card_locks: Mutex<HashMap<Uuid, std::sync::Arc<tokio::sync::Mutex<()>>>>,
}

impl Default for DiscordState {
    fn default() -> Self {
        Self::new()
    }
}

impl DiscordState {
    pub fn new() -> Self {
        Self {
            http: Mutex::new(None),
            owner_channel_id: Mutex::new(None),
            bot_user_id: Mutex::new(None),
            guild_id: Mutex::new(None),
            commands_sig: Mutex::new(None),
            commands_watcher_started: Mutex::new(false),
            session_channels: Mutex::new(HashMap::new()),
            channel_sessions: Mutex::new(HashMap::new()),
            pending_approvals: Mutex::new(HashMap::new()),
            cancel_tokens: Mutex::new(HashMap::new()),
            pending_selects: Mutex::new(HashMap::new()),
            pending_forms: Mutex::new(HashMap::new()),
            long_answers: Mutex::new((Vec::new(), HashMap::new())),
            tool_groups: Mutex::new((Vec::new(), HashMap::new())),
            plan_cards: Mutex::new(HashMap::new()),
            plan_card_store: Mutex::new(None),
            plan_card_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Tracked plan card for a session: `(channel_id, message_id, signature)`.
    pub(crate) async fn plan_card(&self, session_id: Uuid) -> Option<(u64, u64, String)> {
        if let Some(hit) = self.plan_cards.lock().await.get(&session_id).cloned() {
            return Some(hit);
        }
        // Miss: either no card, or this process just started and the map is
        // empty. Rehydrate here rather than scanning every session at boot, so
        // the cost is paid once, only for sessions that actually ask (#104).
        let stored = {
            let guard = self.plan_card_store.lock().await;
            let repo = guard.as_ref()?;
            match repo.get(&session_id.to_string()).await {
                Ok(row) => row?,
                Err(e) => {
                    tracing::warn!("Discord plan-card lookup failed for session {session_id}: {e}");
                    return None;
                }
            }
        };
        // Snowflakes round-trip through i64 unchanged; the column types are
        // shared with Telegram's chat / thread ids.
        let card = (
            stored.chat_id as u64,
            stored.message_id as u64,
            stored.signature,
        );
        self.plan_cards
            .lock()
            .await
            .insert(session_id, card.clone());
        tracing::info!("Recovered Discord plan card for session {session_id} after restart");
        Some(card)
    }

    /// Record the card currently on screen for a session. Called after a
    /// successful create OR edit, so the signature always describes what the
    /// chat actually shows.
    pub(crate) async fn set_plan_card(
        &self,
        session_id: Uuid,
        channel_id: u64,
        message_id: u64,
        signature: String,
    ) {
        self.plan_cards
            .lock()
            .await
            .insert(session_id, (channel_id, message_id, signature.clone()));
        // Persist alongside, so a restart can still find and update THIS
        // message instead of posting a second card below the stale one (#104).
        let guard = self.plan_card_store.lock().await;
        if let Some(repo) = guard.as_ref()
            && let Err(e) = repo
                .set(crate::db::repository::PlanCard {
                    session_id: session_id.to_string(),
                    chat_id: channel_id as i64,
                    thread_id: None,
                    message_id: message_id as i64,
                    signature,
                })
                .await
        {
            tracing::warn!("Failed to persist Discord plan card for session {session_id}: {e}");
        }
    }

    /// Forget a session's card (deleted, or the plan is gone). Drops the
    /// durable row too, so a restart does not resurrect a card the chat no
    /// longer shows (#104).
    pub(crate) async fn clear_plan_card(&self, session_id: Uuid) {
        self.plan_cards.lock().await.remove(&session_id);
        let guard = self.plan_card_store.lock().await;
        if let Some(repo) = guard.as_ref()
            && let Err(e) = repo.delete(&session_id.to_string()).await
        {
            tracing::warn!("Failed to clear Discord plan card for session {session_id}: {e}");
        }
    }

    /// Give the plan-card map durable backing (#104). Called at startup,
    /// mirroring Telegram's `set_plan_card_store` (#809).
    pub(crate) async fn set_plan_card_store(
        &self,
        repo: crate::db::repository::PlanCardRepository,
    ) {
        *self.plan_card_store.lock().await = Some(repo);
    }

    /// Per-session write lock, created on first use.
    ///
    /// Mirrors Telegram's `plan_card_lock` (#822): the map read, the
    /// edit-or-post decision and the id write must happen with nothing else
    /// interleaving, or two refreshes race into two cards.
    pub(crate) async fn plan_card_lock(
        &self,
        session_id: Uuid,
    ) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.plan_card_locks.lock().await;
        locks
            .entry(session_id)
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}
