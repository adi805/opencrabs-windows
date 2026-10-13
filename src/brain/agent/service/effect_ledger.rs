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
//!
//! #213: resolving the row is necessary but not sufficient. The opening step
//! now also DECIDES, because "here is the id of an existing row" is not an
//! answer a mutating caller can act on: it would run the effect again and then
//! settle a row that was already settled, leaving the ledger describing the
//! earlier effect. [`open_effect`] therefore returns an [`EffectDecision`], and
//! callers must run the tool only when [`should_execute`] says so. The claim is
//! the insert's own row count (`INSERT OR IGNORE` against the unique partial
//! index), so two concurrent duplicates cannot both win it.

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
) -> EffectDecision {
    let Some(turn_id) = turn_id else {
        return EffectDecision::Unavailable;
    };
    let Some(pool) = crate::db::global_pool() else {
        return EffectDecision::Unavailable;
    };
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

/// What the ledger decided about one tool call, BEFORE the tool runs.
///
/// A caller must not treat "the key already has a row" as permission to run:
/// that is how a settled effect got a second execution and a second ledger
/// record that described the first one. Each variant names a distinct action.
#[derive(Debug)]
pub(crate) enum EffectDecision {
    /// First sight of this key and this caller won the claim. Run the tool,
    /// then settle `id`.
    Fresh(String),
    /// The key already landed with the SAME tool and arguments. Do NOT run it
    /// again; `row.result_preview` is what the first execution produced.
    Landed(Box<crate::db::repository::EffectRow>),
    /// The key exists but names a different tool or different arguments. This
    /// is a collision, not a replay: refuse before executing so the recorded
    /// effect and the call in hand cannot be conflated.
    Rejected {
        existing_tool: String,
        tool_differs: bool,
        args_differ: bool,
    },
    /// The key exists and is still `pending` with matching identity. Either an
    /// owner is mid-flight or a crash left the outcome unknown; both mean the
    /// same thing to a mutating caller: do not run it again silently.
    Pending(Box<crate::db::repository::EffectRow>),
    /// No turn, no pool, or the write failed. The tool still runs, it just
    /// loses crash-replay protection for this call. Never blocks the tool.
    Unavailable,
}

impl EffectDecision {
    /// The row id to settle after a successful run, when this call owns one.
    pub(crate) fn settle_id(&self) -> Option<&str> {
        match self {
            EffectDecision::Fresh(id) => Some(id.as_str()),
            _ => None,
        }
    }
}

/// Whether the tool may run under this decision.
///
/// Only a fresh claim (or a degraded ledger) executes. A landed effect must not
/// re-run, a collision must not run at all, and a pending effect must not be
/// replayed: the ledger cannot prove it did not already land, so it reports the
/// unknown rather than repeating a side effect.
pub(crate) fn should_execute(decision: &EffectDecision) -> bool {
    matches!(
        decision,
        EffectDecision::Fresh(_) | EffectDecision::Unavailable
    )
}

/// A synthetic tool result for a call the ledger refused to execute, so the
/// model sees why instead of a silent gap in its tool results.
pub(crate) fn skipped_result(decision: &EffectDecision) -> crate::brain::tools::ToolResult {
    let (success, output, error) = match decision {
        EffectDecision::Fresh(_) | EffectDecision::Unavailable => (true, String::new(), None),
        EffectDecision::Landed(row) => (
            row.status == "success",
            format!(
                "Effect '{}' already landed (status={}); reusing the recorded result instead of \
                 re-running it. Recorded output: {}",
                row.tool_name,
                row.status,
                row.result_preview.as_deref().unwrap_or("(none recorded)")
            ),
            None,
        ),
        EffectDecision::Pending(row) => (
            false,
            format!(
                "Effect '{}' is still pending (key {}): its outcome is unknown, so it was not \
                 re-run. Resolve the earlier attempt before retrying.",
                row.tool_name,
                row.effect_key.as_deref().unwrap_or("?")
            ),
            Some("effect outcome unknown (pending)".to_string()),
        ),
        EffectDecision::Rejected {
            existing_tool,
            tool_differs,
            args_differ,
        } => {
            let why = match (tool_differs, args_differ) {
                (true, true) => "a different tool and different arguments",
                (true, false) => "a different tool",
                (false, true) => "the same tool with different arguments",
                (false, false) => "a conflicting call",
            };
            (
                false,
                format!(
                    "Refused before execution: this idempotency key is already bound to \
                     '{existing_tool}' with {why}."
                ),
                Some("effect key reused with different tool or arguments".to_string()),
            )
        }
    };
    crate::brain::tools::ToolResult {
        success,
        output,
        error,
        metadata: std::collections::HashMap::new(),
        images: Vec::new(),
    }
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
) -> EffectDecision {
    let key = effect_key(turn_id, tool_use_id);
    let hash = args_hash(tool_input);
    let fresh = uuid::Uuid::new_v4().to_string();
    // The insert is the claim. `INSERT OR IGNORE` against the unique partial
    // index reports 1 when this caller created the row and 0 when the key was
    // already taken, and SQLite decides that atomically, so two concurrent
    // duplicates cannot both read "1".
    let claimed = match repo
        .record_intent(
            &fresh, turn_id, message_id, session_id, tool_name, &key, &hash,
        )
        .await
    {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("[EFFECT] could not open ledger row for '{tool_name}': {e}");
            return EffectDecision::Unavailable;
        }
    };
    if claimed == 1 {
        return EffectDecision::Fresh(fresh);
    }
    // The key was taken. Read the surviving row and decide from its CONTENT,
    // not from the fact that a row exists.
    match repo.find_by_effect_key(&key).await {
        Ok(Some(row)) => {
            let tool_differs = row.tool_name != tool_name;
            let args_differ = row.args_hash.as_deref() != Some(hash.as_str());
            if tool_differs || args_differ {
                tracing::warn!(
                    "[EFFECT] key '{key}' reused for '{tool_name}' but is bound to '{}' \
                     (tool_differs={tool_differs}, args_differ={args_differ}); refusing",
                    row.tool_name
                );
                EffectDecision::Rejected {
                    existing_tool: row.tool_name.clone(),
                    tool_differs,
                    args_differ,
                }
            } else if row.status != "pending" {
                EffectDecision::Landed(Box::new(row))
            } else {
                EffectDecision::Pending(Box::new(row))
            }
        }
        Ok(None) => {
            // The claim failed but the row is gone: treat as degraded rather
            // than inventing ownership of a row we cannot read.
            tracing::warn!("[EFFECT] claim lost for '{key}' but no row is readable");
            EffectDecision::Unavailable
        }
        Err(e) => {
            tracing::warn!("[EFFECT] could not resolve ledger row for '{tool_name}': {e}");
            EffectDecision::Unavailable
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
