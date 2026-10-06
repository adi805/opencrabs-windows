//! Tool Execution Repository
//!
//! Tracks every tool call for usage analytics (Core Tools card in /usage dashboard).
//! Entries are append-only.

use crate::db::Pool;
use crate::db::database::interact_err;
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

/// Aggregated tool usage stats
#[derive(Debug, Clone)]
pub struct ToolUsageStats {
    pub tool_name: String,
    pub call_count: i64,
}

/// Per-tool totals with failure counts, for the Mission Control analytics
/// panel (status `'error'` counts as a failure).
#[derive(Debug, Clone)]
pub struct ToolFailureStats {
    pub tool_name: String,
    pub total: i64,
    pub failures: i64,
}

/// One row of the effect ledger (FR-003). `status` is `pending` before the
/// effect runs and the tool's outcome after; `effect_key` is the idempotency
/// key resume checks before replaying anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRow {
    pub id: String,
    pub turn_id: Option<String>,
    pub tool_name: String,
    pub status: String,
    pub effect_key: Option<String>,
    pub args_hash: Option<String>,
    pub result_preview: Option<String>,
    pub attempt: i64,
    pub committed_at: Option<i64>,
}

fn map_effect(row: &rusqlite::Row<'_>) -> rusqlite::Result<EffectRow> {
    Ok(EffectRow {
        id: row.get("id")?,
        turn_id: row.get("turn_id")?,
        tool_name: row.get("tool_name")?,
        status: row.get("status")?,
        effect_key: row.get("effect_key")?,
        args_hash: row.get("args_hash")?,
        result_preview: row.get("result_preview")?,
        attempt: row.get("attempt")?,
        committed_at: row.get("committed_at")?,
    })
}

/// Repository for tool execution tracking
#[derive(Clone)]
pub struct ToolExecutionRepository {
    pool: Pool,
}

impl ToolExecutionRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Record a tool execution.
    ///
    /// Refuses to write rows with an empty `tool_name`. Historically a model
    /// occasionally emitted `tool_use` blocks with no name field — dispatch
    /// errored, but the failure still got recorded with the empty name,
    /// producing a blank row in the usage dashboard. There's nothing
    /// meaningful to record for an unnamed tool; logging the refusal at warn
    /// level is enough to surface upstream model misbehaviour without
    /// polluting the stats.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        id: &str,
        message_id: &str,
        session_id: &str,
        tool_name: &str,
        status: &str,
        provider: Option<&str>,
        model: Option<&str>,
        duration_ms: Option<i64>,
    ) -> Result<()> {
        if tool_name.trim().is_empty() {
            tracing::warn!(
                "ToolRepo::record skipped: empty tool_name (id={}, message_id={}, status={})",
                id,
                message_id,
                status
            );
            return Ok(());
        }
        // Defense-in-depth: reject garbage tool names that leak from
        // phantom tool calls (reasoning text + stray XML/JSON fragments).
        // Valid tool names are lowercase alphanumeric + underscores, ≤64 chars.
        if tool_name.len() > 64
            || !tool_name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            tracing::warn!(
                "ToolRepo::record skipped: invalid tool_name {:?} (id={}, message_id={}, status={})",
                &tool_name[..tool_name.len().min(80)],
                id,
                message_id,
                status
            );
            return Ok(());
        }
        let id = id.to_string();
        let message_id = message_id.to_string();
        let session_id = session_id.to_string();
        let tool_name = tool_name.to_string();
        let status = status.to_string();
        let provider = provider.map(|s| s.to_string());
        let model = model.map(|s| s.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO tool_executions \
                     (id, message_id, session_id, tool_name, status, provider, model, duration_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        id,
                        message_id,
                        session_id,
                        tool_name,
                        status,
                        provider,
                        model,
                        duration_ms
                    ],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to record tool execution")?;
        Ok(())
    }

    /// Get tool usage stats grouped by tool_name, optionally filtered by time period
    pub async fn stats_by_tool(&self, since_epoch: Option<i64>) -> Result<Vec<ToolUsageStats>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                // Skip rows with empty tool_name (see ToolRepo::record).
                let (query, param): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
                    if let Some(since) = since_epoch {
                        (
                            "SELECT tool_name, COUNT(*) as cnt \
                             FROM tool_executions \
                             WHERE created_at >= ?1 AND tool_name <> '' \
                             GROUP BY tool_name \
                             ORDER BY cnt DESC"
                                .to_string(),
                            vec![Box::new(since)],
                        )
                    } else {
                        (
                            "SELECT tool_name, COUNT(*) as cnt \
                             FROM tool_executions \
                             WHERE tool_name <> '' \
                             GROUP BY tool_name \
                             ORDER BY cnt DESC"
                                .to_string(),
                            vec![],
                        )
                    };
                let mut stmt = conn.prepare_cached(&query)?;
                let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                    param.iter().map(|p| p.as_ref()).collect();
                let rows = stmt.query_map(param_refs.as_slice(), |row| {
                    Ok(ToolUsageStats {
                        tool_name: row.get(0)?,
                        call_count: row.get(1)?,
                    })
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to query tool usage stats")
    }

    /// Per-tool totals and failure counts (`status = 'error'`), optionally
    /// since an epoch. Powers the Mission Control analytics panel's fail-rate
    /// view. Ordered by total calls descending.
    /// Last RSI activity (self_improve / feedback_analyze / rsi_propose
    /// executions) and how many tool events recorded AFTER it (#469): a
    /// stale RSI with a busy ledger is the signal Mission Control surfaces.
    /// `None` last-ts means RSI never ran; events_since then counts all
    /// recorded tool events.
    /// Recent (session_id, tool_name) rows ordered so consecutive rows in a
    /// session are adjacent and in execution order (#504). The caller groups
    /// them into per-session tool sequences for the skill-pattern detector.
    /// Empty tool names are skipped (see `record`).
    pub async fn recent_session_tool_sequences(&self, limit: i64) -> Result<Vec<(String, String)>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT session_id, tool_name FROM tool_executions \
                     WHERE tool_name <> '' AND session_id <> '' \
                     ORDER BY session_id, created_at, id LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![limit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to fetch session tool sequences")
    }

    pub async fn rsi_staleness(&self) -> Result<(Option<i64>, i64)> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| -> rusqlite::Result<(Option<i64>, i64)> {
                let last: Option<i64> = conn.query_row(
                    "SELECT MAX(created_at) FROM tool_executions \
                     WHERE tool_name IN ('self_improve','feedback_analyze','rsi_propose')",
                    [],
                    |r| r.get(0),
                )?;
                let since: i64 = match last {
                    Some(ts) => conn.query_row(
                        "SELECT COUNT(*) FROM tool_executions WHERE created_at > ?1",
                        rusqlite::params![ts],
                        |r| r.get(0),
                    )?,
                    None => {
                        conn.query_row("SELECT COUNT(*) FROM tool_executions", [], |r| r.get(0))?
                    }
                };
                Ok((last, since))
            })
            .await
            .map_err(interact_err)?
            .context("Failed to query RSI staleness")
    }

    pub async fn stats_with_failures(
        &self,
        since_epoch: Option<i64>,
    ) -> Result<Vec<ToolFailureStats>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let (query, param): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
                    if let Some(since) = since_epoch {
                        (
                            "SELECT tool_name, COUNT(*) AS total, \
                                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS fails \
                             FROM tool_executions \
                             WHERE created_at >= ?1 AND tool_name <> '' \
                             GROUP BY tool_name ORDER BY total DESC"
                                .to_string(),
                            vec![Box::new(since)],
                        )
                    } else {
                        (
                            "SELECT tool_name, COUNT(*) AS total, \
                                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS fails \
                             FROM tool_executions \
                             WHERE tool_name <> '' \
                             GROUP BY tool_name ORDER BY total DESC"
                                .to_string(),
                            vec![],
                        )
                    };
                let mut stmt = conn.prepare_cached(&query)?;
                let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                    param.iter().map(|p| p.as_ref()).collect();
                let rows = stmt.query_map(param_refs.as_slice(), |row| {
                    Ok(ToolFailureStats {
                        tool_name: row.get(0)?,
                        total: row.get(1)?,
                        failures: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    })
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to query tool failure stats")
    }

    // ── Effect ledger (FR-003) ─────────────────────────────────────────
    //
    // The analytics rows above are written once, after the fact. The ledger is
    // two-phase: the intent is recorded BEFORE the effect runs and settled
    // AFTER, so a resumed turn can tell "already landed" from "not yet" and
    // never replay a side effect that already happened.

    /// Record a tool call's intent before its effect runs. `effect_key` is the
    /// idempotency key: resume treats a row already settled as landed and
    /// skips it.
    #[allow(clippy::too_many_arguments)] // one INSERT: id, turn, message, session, tool, key, hash
    pub async fn record_intent(
        &self,
        id: &str,
        turn_id: &str,
        message_id: &str,
        session_id: &str,
        tool_name: &str,
        effect_key: &str,
        args_hash: &str,
    ) -> Result<()> {
        let id = id.to_string();
        let turn_id = turn_id.to_string();
        let message_id = message_id.to_string();
        let session_id = session_id.to_string();
        let tool_name = tool_name.to_string();
        let effect_key = effect_key.to_string();
        let args_hash = args_hash.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO tool_executions \
                     (id, message_id, session_id, tool_name, status, turn_id, effect_key, args_hash) \
                     VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7)",
                    params![id, message_id, session_id, tool_name, turn_id, effect_key, args_hash],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to record effect intent")?;
        Ok(())
    }

    /// Settle an effect opened by [`Self::record_intent`]. Returns the number
    /// of rows changed: `0` means the row was already settled (or never
    /// opened), so the caller must not treat the effect as newly done.
    pub async fn settle(
        &self,
        id: &str,
        status: &str,
        result_preview: Option<&str>,
        duration_ms: Option<i64>,
    ) -> Result<usize> {
        let id = id.to_string();
        let status = status.to_string();
        let preview = result_preview.map(|s| s.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE tool_executions \
                     SET status = ?2, result_preview = ?3, duration_ms = ?4, \
                         committed_at = strftime('%s', 'now') \
                     WHERE id = ?1 AND status = 'pending'",
                    params![id, status, preview, duration_ms],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to settle effect")
    }

    /// The ledger row for an idempotency key, or `None` when the key is new.
    pub async fn find_by_effect_key(&self, effect_key: &str) -> Result<Option<EffectRow>> {
        let key = effect_key.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT id, turn_id, tool_name, status, effect_key, args_hash, \
                            result_preview, attempt, committed_at \
                     FROM tool_executions WHERE effect_key = ?1 LIMIT 1",
                    params![key],
                    map_effect,
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read effect by key")
    }

    /// Effects a turn opened that never settled. Resume uses this to decide
    /// what is unknown rather than replaying everything.
    pub async fn pending_for_turn(&self, turn_id: &str) -> Result<Vec<EffectRow>> {
        let tid = turn_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare(
                    "SELECT id, turn_id, tool_name, status, effect_key, args_hash, \
                            result_preview, attempt, committed_at \
                     FROM tool_executions WHERE turn_id = ?1 AND status = 'pending' \
                     ORDER BY created_at ASC, rowid ASC",
                )?
                .query_map(params![tid], map_effect)?
                .collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to list pending effects")
    }
}
