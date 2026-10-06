//! Atomic turn commit (FR-002).
//!
//! One turn is one durability unit. Everything the turn produces — the
//! assistant message's final content and usage, the session's running totals,
//! the cumulative usage ledger row and the turn journal's settlement — lands
//! in a SINGLE SQLite transaction. A process killed anywhere before that
//! transaction commits leaves the turn untouched, so reopening the storage
//! always yields either a complete turn or none.
//!
//! The turn journal's state is the guard: the transaction only commits if the
//! turn is still `running`. A turn already reconciled to `interrupted` (boot
//! saw it open, meaning the process that owned it died) cannot be committed by
//! a late settle, so a zombie turn cannot double-count usage.

use crate::db::repository::usage_ledger::normalize_model_name;
use crate::db::{Pool, database::interact_err};
use anyhow::{Context, Result, bail};
use rusqlite::params;
use uuid::Uuid;

/// Everything a turn produces, committed together.
#[derive(Debug, Clone)]
pub struct TurnCommit<'a> {
    /// The `turns` row opened at turn start.
    pub turn_id: &'a str,
    /// The session the turn belongs to.
    pub session_id: Uuid,
    /// The assistant message whose usage is being finalised.
    pub message_id: Uuid,
    /// Total tokens billed for the turn.
    pub token_count: i64,
    /// Cost in USD for the turn.
    pub cost: f64,
    /// Server-reported prompt tokens (drives the UI context meter).
    pub input_tokens: Option<i64>,
    pub cache_creation_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    /// Wall-clock seconds the turn took.
    pub duration_secs: Option<i64>,
    /// The provider and model that actually served the turn.
    pub provider: &'a str,
    pub model: &'a str,
}

/// Commit a turn in one transaction. Returns `Err` when the turn is no longer
/// `running` — in that case NOTHING is written, not even the message usage,
/// because the turn was already reconciled as interrupted.
pub async fn commit_turn(pool: &Pool, commit: TurnCommit<'_>) -> Result<()> {
    let turn_id = commit.turn_id.to_string();
    let session_id = commit.session_id.to_string();
    let message_id = commit.message_id.to_string();
    let provider = commit.provider.to_string();
    let model = normalize_model_name(commit.model);
    let token_count = commit.token_count;
    let cost = commit.cost;
    let input_tokens = commit.input_tokens;
    let cache_creation_tokens = commit.cache_creation_tokens;
    let cache_read_tokens = commit.cache_read_tokens;
    let duration_secs = commit.duration_secs;

    let settled = pool
        .get()
        .await
        .context("Failed to get connection")?
        .interact(move |conn| -> rusqlite::Result<bool> {
            let tx = conn.transaction()?;

            // 1. Finalise the assistant message's usage. COALESCE keeps any
            //    value already recorded when this call passes None.
            tx.execute(
                "UPDATE messages SET \
                   token_count = ?2, cost = ?3, \
                   input_tokens = COALESCE(?4, input_tokens), \
                   cache_creation_tokens = COALESCE(?5, cache_creation_tokens), \
                   cache_read_tokens = COALESCE(?6, cache_read_tokens), \
                   duration_secs = COALESCE(?7, duration_secs) \
                 WHERE id = ?1",
                params![
                    message_id,
                    token_count,
                    cost,
                    input_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                    duration_secs,
                ],
            )?;

            // 2. Move the session's running totals in the same transaction.
            tx.execute(
                "UPDATE sessions SET \
                   token_count = token_count + ?2, \
                   total_cost = total_cost + ?3, \
                   updated_at = strftime('%s', 'now') \
                 WHERE id = ?1",
                params![session_id, token_count, cost],
            )?;

            // 3. Append the cumulative ledger row.
            tx.execute(
                "INSERT INTO usage_ledger (session_id, provider, model, token_count, cost) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, provider, model, token_count, cost],
            )?;

            // 4. Settle the journal LAST. Zero rows changed means the turn was
            //    already reconciled or committed; return without committing so
            //    the whole transaction rolls back.
            let changed = tx.execute(
                "UPDATE turns SET state = 'committed', committed_at = strftime('%s', 'now') \
                 WHERE id = ?1 AND state = 'running'",
                params![turn_id],
            )?;
            if changed == 0 {
                return Ok(false);
            }

            tx.commit()?;
            Ok(true)
        })
        .await
        .map_err(interact_err)??;

    if !settled {
        bail!("turn {turn_id} is not running; refusing to commit (already settled or interrupted)");
    }
    Ok(())
}
