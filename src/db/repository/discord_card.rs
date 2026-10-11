//! Durable Discord card state (H1 / M4).
//!
//! The tool-group store (#380) and the long-answer pager store (FR-009) lived
//! only in `DiscordState`'s in-process maps, while the plan card next to them
//! had a real durable backing (`plan_cards`, #104/#809). A restart therefore
//! left the card on screen with a working-looking button and no way to answer
//! it: Expand replied "This status bubble is too old to expand", and a pager
//! arrow replied "That pager aged out". The card could not be edited either,
//! because nothing knew which message carried it.
//!
//! One row per card, keyed by the message id its own button carries. `kind`
//! separates the two stores that share the table; `state` is opaque here and
//! is the JSON each store owns. Rows are deleted by the store when it evicts
//! the card from memory, so the table tracks the live cards and nothing else.

use anyhow::{Context, Result};
use deadpool_sqlite::Pool;
use rusqlite::OptionalExtension;
use rusqlite::params;

use crate::db::database::interact_err;

/// A card's stored state, as persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordCardRow {
    pub message_id: i64,
    /// `tool_group` or `long_answer`.
    pub kind: String,
    /// The store's own JSON snapshot.
    pub state: String,
}

pub struct DiscordCardRepository {
    pool: Pool,
}

impl DiscordCardRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// The stored snapshot for one card, if it is still tracked.
    pub async fn get(&self, message_id: i64, kind: &str) -> Result<Option<String>> {
        let kind = kind.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT state FROM discord_card_state \
                     WHERE message_id = ?1 AND kind = ?2",
                    params![message_id, kind],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read Discord card state")
    }

    /// Track (or re-track) a card. Upsert, so a re-render rewrites the row
    /// instead of failing on the primary key.
    pub async fn set(&self, row: DiscordCardRow) -> Result<()> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO discord_card_state \
                       (message_id, kind, state, updated_at) \
                     VALUES (?1, ?2, ?3, strftime('%s','now')) \
                     ON CONFLICT(message_id) DO UPDATE SET \
                       kind = excluded.kind, \
                       state = excluded.state, \
                       updated_at = excluded.updated_at",
                    params![row.message_id, row.kind, row.state],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to write Discord card state")?;
        Ok(())
    }

    /// Stop tracking a card. Called in lockstep with the in-process eviction,
    /// so the table never accumulates rows for cards the map has dropped.
    pub async fn delete(&self, message_id: i64) -> Result<()> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "DELETE FROM discord_card_state WHERE message_id = ?1",
                    params![message_id],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to delete Discord card state")?;
        Ok(())
    }
}
