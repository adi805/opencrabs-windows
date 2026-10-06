//! Submission Repository
//!
//! FR-004: a submit carrying a `requestId` is idempotent. Submitting the same
//! id twice returns the existing row instead of starting a second run, which
//! is what a mobile client needs when it retries across a dropped connection
//! or re-attaches after the process was restarted. Concurrency-safe by
//! construction: the primary key plus `INSERT OR IGNORE` means two racing
//! claims of one id converge on a single row, and the loser reads the
//! winner's.

use crate::db::{Pool, database::interact_err};
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

/// Claimed but not started.
pub const SUBMISSION_QUEUED: &str = "queued";
/// A run is in flight for this submission.
pub const SUBMISSION_RUNNING: &str = "running";
/// The run finished and its answer is committed.
pub const SUBMISSION_DONE: &str = "done";
/// The run ended in an error.
pub const SUBMISSION_FAILED: &str = "failed";

/// One submission row, keyed by the caller's `requestId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub request_id: String,
    pub session_id: String,
    pub state: String,
    pub message_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn map_submission(row: &rusqlite::Row<'_>) -> rusqlite::Result<Submission> {
    Ok(Submission {
        request_id: row.get("request_id")?,
        session_id: row.get("session_id")?,
        state: row.get("state")?,
        message_id: row.get("message_id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// Reads and writes [`Submission`] rows.
#[derive(Clone)]
pub struct SubmissionRepository {
    pool: Pool,
}

impl SubmissionRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Claim a request id for a session. Returns the row plus whether this
    /// call created it: `true` means the caller owns the run, `false` means
    /// the id was already known and the caller must not start a second one.
    ///
    /// Read and insert share one transaction, so two concurrent claims of the
    /// same id cannot both report `true`.
    pub async fn claim(&self, request_id: &str, session_id: &str) -> Result<(Submission, bool)> {
        let rid = request_id.to_string();
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let tx = conn.transaction()?;
                let created = tx.execute(
                    "INSERT OR IGNORE INTO submissions (request_id, session_id, state) \
                     VALUES (?1, ?2, ?3)",
                    params![rid, sid, SUBMISSION_QUEUED],
                )? == 1;
                let row = tx.query_row(
                    "SELECT request_id, session_id, state, message_id, created_at, updated_at \
                     FROM submissions WHERE request_id = ?1",
                    params![rid],
                    map_submission,
                )?;
                tx.commit()?;
                Ok((row, created))
            })
            .await
            .map_err(interact_err)?
            .context("Failed to claim submission")
    }

    /// Move a submission to `state`, optionally stamping the message it
    /// produced. A `None` message id leaves any earlier stamp in place, so a
    /// later state change does not erase the link to the answer.
    pub async fn set_state(
        &self,
        request_id: &str,
        state: &str,
        message_id: Option<&str>,
    ) -> Result<()> {
        let rid = request_id.to_string();
        let st = state.to_string();
        let mid = message_id.map(|s| s.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE submissions \
                     SET state = ?2, message_id = COALESCE(?3, message_id), \
                         updated_at = strftime('%s', 'now') \
                     WHERE request_id = ?1",
                    params![rid, st, mid],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to update submission")?;
        Ok(())
    }

    /// One submission by its request id, or `None` if it was never claimed.
    pub async fn find_by_request_id(&self, request_id: &str) -> Result<Option<Submission>> {
        let rid = request_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT request_id, session_id, state, message_id, created_at, updated_at \
                     FROM submissions WHERE request_id = ?1",
                    params![rid],
                    map_submission,
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read submission")
    }

    /// Every submission for a session, newest first.
    pub async fn list_for_session(&self, session_id: &str) -> Result<Vec<Submission>> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare(
                    "SELECT request_id, session_id, state, message_id, created_at, updated_at \
                     FROM submissions WHERE session_id = ?1 ORDER BY created_at DESC",
                )?
                .query_map(params![sid], map_submission)?
                .collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to list submissions")
    }
}
