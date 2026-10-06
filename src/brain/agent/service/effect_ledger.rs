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
//!
//! Why the row id is RESOLVED rather than remembered: the idempotency key, not
//! the uuid, is the identity of an effect. When a resumed turn re-issues a call
//! that already has a row, the insert is ignored and the original row survives
//! under its original id. A handler that kept the fresh uuid would then settle
//! a row that does not exist, leaving the effect that really landed stuck
//! `pending` forever: the ledger would report an effect as unknown precisely
//! when it is known. So both halves look the row up by key.

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

/// Open an effect before its side effect runs, returning the canonical row id.
///
/// The returned id is read back from storage, not the one generated here. On a
/// first run they are the same uuid; on a replay the insert is ignored and this
/// returns the ORIGINAL row's id, so the settle that follows still lands on the
/// row that exists. Callers must pass this value to [`settle_effect`] rather
/// than keeping an id of their own.
///
/// `None` means the ledger is degraded (no turn journal, no pool, or the write
/// failed), never that the tool must be skipped: the tool still runs, it just
/// loses crash-replay protection for this call.
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
    open_effect_with(
        &repo,
        turn_id,
        message_id,
        session_id,
        tool_name,
        tool_use_id,
        tool_input,
    )
    .await
}

/// [`open_effect`] against an explicit repository, so the replay contract is
/// testable without the process-wide pool.
pub(crate) async fn open_effect_with(
    repo: &crate::db::repository::ToolExecutionRepository,
    turn_id: &str,
    message_id: &str,
    session_id: &str,
    tool_name: &str,
    tool_use_id: &str,
    tool_input: &serde_json::Value,
) -> Option<String> {
    let key = effect_key(turn_id, tool_use_id);
    let hash = args_hash(tool_input);
    let fresh = uuid::Uuid::new_v4().to_string();
    match repo
        .record_intent(
            &fresh, turn_id, message_id, session_id, tool_name, &key, &hash,
        )
        .await
    {
        Ok(()) => {}
        Err(e) => {
            tracing::warn!("[EFFECT] could not open ledger row for '{tool_name}': {e}");
            return None;
        }
    }
    // The insert above is `INSERT OR IGNORE` against a unique `effect_key`, so
    // a replay leaves the original row in place. Read the key back to learn
    // which row that is.
    match repo.find_by_effect_key(&key).await {
        Ok(Some(row)) => Some(row.id),
        Ok(None) => Some(fresh),
        Err(e) => {
            tracing::warn!("[EFFECT] could not resolve ledger row for '{tool_name}': {e}");
            Some(fresh)
        }
    }
}

/// Settle an effect opened by [`open_effect`].
///
/// `effect_id` is the canonical row id that `open_effect` returned. A settle
/// that changes zero rows is not an error: it means the row was already
/// settled, and the caller must not treat the effect as newly done.
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
