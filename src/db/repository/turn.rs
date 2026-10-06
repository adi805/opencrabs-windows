//! Turn Journal Repository
//!
//! FR-002: a turn is one durability unit. The row is opened when the turn
//! starts and settled in the same SQLite transaction that writes the
//! assistant message, its tool executions and its usage, so a process kill
//! mid-turn leaves a complete turn or none — never a torn one. Boot scans for
//! rows still in `running` and marks them `interrupted`, which is the signal
//! that a process died mid-turn.

use crate::db::{Pool, database::interact_err};
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

/// The turn is in flight. A `running` row seen at boot means the process that
/// opened it died before settling it.
pub const TURN_RUNNING: &str = "running";
/// The turn's writes landed in one commit.
pub const TURN_COMMITTED: &str = "committed";
/// The process died mid-turn and boot reconciled the row.
pub const TURN_INTERRUPTED: &str = "interrupted";
/// The turn ended in an error it recorded itself.
pub const TURN_FAILED: &str = "failed";

/// One turn journal row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRow {
    pub id: String,
    pub session_id: String,
    pub state: String,
    pub started_at: i64,
    pub committed_at: Option<i64>,
    pub error: Option<String>,
}

fn map_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnRow> {
    Ok(TurnRow {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        state: row.get("state")?,
        started_at: row.get("started_at")?,
        committed_at: row.get("committed_at")?,
        error: row.get("error")?,
    })
}

/// Reads and writes [`TurnRow`] rows.
#[derive(Clone)]
pub struct TurnRepository {
    pool: Pool,
}

impl TurnRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Open a turn and return its id. The caller settles it with
    /// [`Self::commit`] or [`Self::fail`].
    pub async fn open(&self, session_id: Uuid) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let row_id = id.clone();
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO turns (id, session_id, state) VALUES (?1, ?2, ?3)",
                    params![row_id, sid, TURN_RUNNING],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to open turn")?;
        Ok(id)
    }

    /// Settle a turn as committed. A no-op when the id is unknown, so a
    /// double-settle cannot resurrect a reconciled row.
    pub async fn commit(&self, turn_id: &str) -> Result<()> {
        let id = turn_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE turns SET state = ?2, committed_at = strftime('%s', 'now') \
                     WHERE id = ?1 AND state = ?3",
                    params![id, TURN_COMMITTED, TURN_RUNNING],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to commit turn")?;
        Ok(())
    }

    /// Settle a turn as failed with a message.
    pub async fn fail(&self, turn_id: &str, error: &str) -> Result<()> {
        let id = turn_id.to_string();
        let err = error.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE turns SET state = ?2, committed_at = strftime('%s', 'now'), error = ?3 \
                     WHERE id = ?1 AND state = ?4",
                    params![id, TURN_FAILED, err, TURN_RUNNING],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to fail turn")?;
        Ok(())
    }

    /// Mark every `running` turn `interrupted` and return the rows that were
    /// reconciled. Read and update share one transaction so the returned rows
    /// are exactly the ones this call settled. Called once at boot.
    pub async fn reconcile_running(&self) -> Result<Vec<TurnRow>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| -> rusqlite::Result<Vec<TurnRow>> {
                let tx = conn.transaction()?;
                let rows: Vec<TurnRow> = {
                    let mut stmt = tx.prepare(
                        "SELECT id, session_id, state, started_at, committed_at, error \
                         FROM turns WHERE state = ?1 ORDER BY started_at ASC, rowid ASC",
                    )?;
                    stmt.query_map(params![TURN_RUNNING], map_turn)?
                        .collect::<std::result::Result<Vec<_>, _>>()?
                };
                if !rows.is_empty() {
                    tx.execute(
                        "UPDATE turns SET state = ?1, error = 'process died mid-turn' \
                         WHERE state = ?2",
                        params![TURN_INTERRUPTED, TURN_RUNNING],
                    )?;
                }
                tx.commit()?;
                // Report the settled state, not the pre-update one: the caller
                // is asking which turns this boot reconciled.
                let reconciled = rows
                    .into_iter()
                    .map(|mut row| {
                        row.state = TURN_INTERRUPTED.to_string();
                        row.error = Some("process died mid-turn".to_string());
                        row
                    })
                    .collect();
                Ok(reconciled)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to reconcile running turns")
    }

    /// One turn row by id.
    pub async fn find_by_id(&self, turn_id: &str) -> Result<Option<TurnRow>> {
        let id = turn_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT id, session_id, state, started_at, committed_at, error \
                     FROM turns WHERE id = ?1",
                    params![id],
                    map_turn,
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read turn")
    }

    /// The most recent turn for a session, whatever its state.
    pub async fn latest_for_session(&self, session_id: Uuid) -> Result<Option<TurnRow>> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT id, session_id, state, started_at, committed_at, error \
                     FROM turns WHERE session_id = ?1 ORDER BY started_at DESC, rowid DESC LIMIT 1",
                    params![sid],
                    map_turn,
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read latest turn")
    }
}
