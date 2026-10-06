//! FR-003: tool effect ledger wiring.
//!
//! The ledger is two-phase. The *intent* of a tool call is committed BEFORE the
//! side effect runs and settled AFTER, keyed by a stable idempotency key
//! (`<turn_id>:<tool_use_id>`). A turn that resumes after a crash can then tell
//! "this side effect already landed" from "this one never ran", instead of
//! replaying an external effect that already happened.
//!
//! Why the intent write is awaited rather than spawned: the guarantee only
//! holds if the row exists on disk before the effect. A fire-and-forget write
//! can lose the race against a crash and leave the effect invisible, which is
//! exactly the failure mode the ledger exists to prevent.

use sha2::{Digest, Sha256};

/// Stable idempotency key for one tool call inside one turn.
///
/// `tool_use_id` is assigned by the provider, so it survives a restart: the
/// same call replayed after a crash produces the same key, and that is what
/// lets the ledger suppress a duplicate effect.
pub(crate) fn effect_key(turn_id: &str, tool_use_id: &str) -> String {
    format!("{turn_id}:{tool_use_id}")
}

/// sha256 of the serialized tool input, so a resume can distinguish "same key,
/// same call" from "same key, different arguments".
pub(crate) fn args_hash(input: &serde_json::Value) -> String {
    let mut h = Sha256::new();
    h.update(input.to_string().as_bytes());
    format!("{:x}", h.finalize())
}

/// Open an effect before its side effect runs.
///
/// Returns the ledger row id to hand to [`settle_effect`], or `None` when the
/// turn has no journal (the open failed) or no pool is available. A `None`
/// here means the ledger is degraded, never that the tool must be skipped: the
/// tool still runs, it just loses crash-replay protection for this call.
pub(crate) async fn open_effect(
    turn_id: Option<&str>,
    message_id: &str,
    session_id: &str,
    tool_name: &str,
    tool_use_id: &str,
    tool_input: &serde_json::Value,
) -> Option<String> {
    let turn_id = turn_id?;
    let pool = crate::db::global_pool()?;
    let repo = crate::db::repository::ToolExecutionRepository::new(pool.clone());
    let id = uuid::Uuid::new_v4().to_string();
    let key = effect_key(turn_id, tool_use_id);
    let hash = args_hash(tool_input);
    match repo
        .record_intent(&id, turn_id, message_id, session_id, tool_name, &key, &hash)
        .await
    {
        Ok(()) => Some(id),
        Err(e) => {
            tracing::warn!("[EFFECT] could not open ledger row for '{tool_name}': {e}");
            None
        }
    }
}

/// Settle an effect opened by [`open_effect`].
///
/// A settle that changes zero rows is not an error: it means the row was
/// already settled (a replay) or never opened, and either way the caller must
/// not treat the effect as newly done.
pub(crate) async fn settle_effect(
    effect_id: &str,
    status: &str,
    result_preview: Option<&str>,
    duration_ms: Option<i64>,
) {
    let Some(pool) = crate::db::global_pool() else {
        return;
    };
    let repo = crate::db::repository::ToolExecutionRepository::new(pool.clone());
    if let Err(e) = repo
        .settle(effect_id, status, result_preview, duration_ms)
        .await
    {
        tracing::warn!("[EFFECT] could not settle ledger row {effect_id}: {e}");
    }
}
