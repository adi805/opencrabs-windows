//! Per-session cancel tokens so `/stop` can abort an in-flight agent call.

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::SlackState;

impl SlackState {
    /// Store a cancel token for a session (before starting agent call).
    ///
    /// Overwrites any prior token WITHOUT cancelling it (#652's rule, the
    /// Slack twin, ported by #298). The cancel that used to live here predates
    /// the atomic `try_begin_turn` gate (`turns.rs`), which is now the single
    /// source of truth for one-turn-per-session, so all it could still reach
    /// was the token a COMPLETED turn left behind: a finished turn never
    /// cancels its own token, `remove_cancel_token` keeps non-cancelled ones
    /// on purpose, and so every new turn logged "cancelling previous in-flight
    /// agent call" for a turn that had ended minutes earlier while killing
    /// nothing. A genuine cancellation goes through `cancel_session` (/stop),
    /// never here.
    pub async fn store_cancel_token(&self, session_id: Uuid, token: CancellationToken) {
        self.cancel_tokens.lock().await.insert(session_id, token);
    }

    /// Cancel and remove the token for a session. Returns true if a token existed.
    pub async fn cancel_session(&self, session_id: Uuid) -> bool {
        if let Some(token) = self.cancel_tokens.lock().await.remove(&session_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Remove the cancel token after the agent call completes (cleanup).
    /// Only removes if the stored token is already cancelled — prevents a
    /// finishing old call from removing a newer call's live token.
    pub async fn remove_cancel_token(&self, session_id: Uuid) {
        let mut tokens = self.cancel_tokens.lock().await;
        if let Some(token) = tokens.get(&session_id)
            && token.is_cancelled()
        {
            tokens.remove(&session_id);
        }
    }
}
