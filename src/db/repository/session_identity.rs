//! Session Identity Repository
//!
//! FR-005: the provider-side session id, persisted so prompt cache and session
//! affinity survive reopen, retry, reset and model changes. This is the analog
//! of Pi Durable's `pi.provider`: one stable opaque id minted per session and
//! replaced only when the session is forked. Without it every restart loses
//! the provider's cached prompt and pays full price for the next turn.

use crate::db::{Pool, database::interact_err};
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

/// Reads and writes the `session_identity` table.
#[derive(Clone)]
pub struct SessionIdentityRepository {
    pool: Pool,
}

impl SessionIdentityRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// The provider session id for `session_id`, minting and persisting one on
    /// first use. Stable across restarts and model changes; only [`Self::set`]
    /// (used by fork) replaces it.
    ///
    /// Insert-then-read shares one transaction, so two concurrent callers for
    /// a brand-new session converge on one id instead of each minting its own.
    pub async fn ensure(&self, session_id: &str) -> Result<String> {
        let sid = session_id.to_string();
        let candidate = Uuid::new_v4().to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| -> rusqlite::Result<String> {
                let tx = conn.transaction()?;
                tx.execute(
                    "INSERT OR IGNORE INTO session_identity (session_id, provider_session_id) \
                     VALUES (?1, ?2)",
                    params![sid, candidate],
                )?;
                let id: String = tx.query_row(
                    "SELECT provider_session_id FROM session_identity WHERE session_id = ?1",
                    params![sid],
                    |row| row.get(0),
                )?;
                tx.commit()?;
                Ok(id)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to ensure session identity")
    }

    /// The stored provider session id, or `None` when the session has none yet.
    pub async fn get(&self, session_id: &str) -> Result<Option<String>> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT provider_session_id FROM session_identity WHERE session_id = ?1",
                    params![sid],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read session identity")
    }

    /// Replace the provider session id. Used by fork so the child starts a
    /// fresh provider session instead of sharing the parent's cache.
    pub async fn set(&self, session_id: &str, provider_session_id: &str) -> Result<()> {
        let sid = session_id.to_string();
        let pid = provider_session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO session_identity (session_id, provider_session_id) \
                     VALUES (?1, ?2) \
                     ON CONFLICT(session_id) DO UPDATE SET \
                       provider_session_id = excluded.provider_session_id, \
                       updated_at = strftime('%s', 'now')",
                    params![sid, pid],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to set session identity")?;
        Ok(())
    }
}
