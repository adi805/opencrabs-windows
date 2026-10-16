//! Usage Ledger Repository
//!
//! Cumulative usage tracking that persists across session deletes and compaction.
//!
//! Entries are append-only and never deleted: nothing rewrites a row, nothing
//! ages one out, and a session purge leaves them in place. That last one is
//! deliberate rather than incidental (F-DB-03). The ledger is cumulative
//! accounting, not session data: every query over it (`totals`,
//! `stats_by_model`, `by_provider`) is all-time and joins nothing, so a purged
//! session's rows are still the only record of what that session cost, and
//! deleting them would shrink the all-time total silently.

use crate::db::Pool;
use crate::db::database::interact_err;
use anyhow::{Context, Result};
use rusqlite::params;

/// Aggregated usage stats grouped by model
#[derive(Debug, Clone)]
pub struct ModelUsageStats {
    pub model: String,
    pub total_tokens: i64,
    pub total_cost: f64,
    pub entry_count: i64,
}

/// The canonical model families, as `(canonical, aliases)`.
///
/// ONE table, and the only place a family is spelled out. `normalize_model_name`
/// matches a name against it, and `stats_by_model` groups by what that function
/// returns, so the writer and the reader cannot disagree about what a model is
/// called (F-DB-04).
///
/// They did disagree. The aggregate carried its own copy of this mapping in a
/// hand-written SQL `CASE`, and it had drifted three ways: `k3`/`kimi-k3` and
/// `zai` were in the Rust list and absent from the SQL; the provider-prefix strip
/// took everything after the FIRST `/` in SQL and the last segment in Rust; and
/// the fallback for an unrecognised name returned the suffix-stripped form in SQL
/// against the raw lowercased form in Rust. The visible effect was one model
/// showing twice in `/usage`, since a row recorded as `k3` became `kimi-k3` on
/// the way in and stayed `k3` on the way out.
pub(crate) const MODEL_FAMILIES: &[(&str, &[&str])] = &[
    // Claude
    ("opus-4-6", &["opus", "opus-4-6"]),
    ("sonnet-4-6", &["sonnet", "sonnet-4-6"]),
    ("haiku-4-5", &["haiku", "haiku-4-5", "haiku-4-5-20251001"]),
    // Qwen
    (
        "qwen3.6-max-preview",
        &[
            "qwen-3.6-max-preview",
            "qwen3.6-max-preview",
            "qwen-3-6-max-preview",
            "qwen3-6-max-preview",
            "qwen-max-preview",
        ],
    ),
    (
        "qwen3.6-plus",
        &["coder-model", "qwen-3.6-plus", "qwen3.6-plus"],
    ),
    ("qwen3.5-plus", &["qwen3.5-plus", "qwen-3.5-plus"]),
    // MiniMax
    ("minimax-m2.5", &["minimax-m2.5"]),
    ("minimax-m2.7", &["minimax-m2.7"]),
    ("minimax-m3", &["minimax-m3"]),
    // Mimo (free variants merge with the base)
    ("mimo-v2-omni", &["mimo-v2-omni", "mimo-v2-omni-free"]),
    ("mimo-v2-pro", &["mimo-v2-pro", "mimo-v2-pro-free"]),
    // Kimi
    ("kimi-k3", &["k3", "kimi-k3"]),
    ("kimi-k2.6", &["kimi-k2.6", "kimi-k2-6", "kimik2.6"]),
    ("kimi-k2.5", &["kimi-k2.5", "kimi-k2-5"]),
    // GLM / ZhiPu — 5.1 and turbo stay separate
    ("glm-5.1", &["glm-5.1", "glm-5-1", "glm-5"]),
    ("glm-5-turbo", &["glm-5-turbo", "zhipu", "zai"]),
];

/// The canonical name for a family alias, or `None` when the name is not one.
pub(crate) fn canonical_model_family(base: &str) -> Option<&'static str> {
    MODEL_FAMILIES
        .iter()
        .find(|(_, aliases)| aliases.contains(&base))
        .map(|(canonical, _)| *canonical)
}

/// Normalize model names for consistent ledger tracking.
/// - Claude: "claude-opus-4-6" → "opus-4-6", bare "opus" → "opus-4-6"
/// - Qwen: "coder-model", "qwen3.6-plus", "qwen-3.6-plus", "qwen/qwen3.6-plus" → "qwen3.6-plus"
/// - OpenRouter/OpenCode prefixed: "openrouter/X", "opencode/X" → "X"
pub(crate) fn normalize_model_name(model: &str) -> String {
    // Strip provider prefixes (openrouter/model, opencode/model, qwen/model, etc.)
    let stripped = model.split('/').next_back().unwrap_or(model);

    // Lowercase everything for consistent matching and display
    let lower = stripped.to_lowercase();

    // Strip a "-@botname" / "@botname" attribution suffix that leaked into the
    // model name (multi-bot usage tagging), so the same model is not split into
    // separate ledger rows — e.g. `qwen3.8-max-preview-@adicrabs_bot` collapses
    // to `qwen3.8-max-preview` (#697). A model identity never carries a handle.
    let lower = match lower.split_once('@') {
        Some((before, _)) => before.trim_end_matches('-').to_string(),
        None => lower,
    };

    // Strip common suffixes before matching: :free, -free, -thinking
    let base = lower
        .strip_suffix(":free")
        .or_else(|| lower.strip_suffix("-free"))
        .or_else(|| lower.strip_suffix("-thinking"))
        .unwrap_or(&lower);

    // Strip claude- prefix
    let base = base.strip_prefix("claude-").unwrap_or(base);

    canonical_model_family(base)
        .map(str::to_string)
        .unwrap_or_else(|| lower.clone())
}

/// Repository for usage ledger operations
#[derive(Clone)]
pub struct UsageLedgerRepository {
    pool: Pool,
}

impl UsageLedgerRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Record a usage event (append-only, never deleted). `provider` is the
    /// serving provider's name at record time (#402): the ledger outlives
    /// sessions, so provider attribution must live on the row itself.
    /// `cache_read_tokens` is the provider-reported cache-read count for the
    /// turn, or `None` when nothing reported one, which keeps the row out of
    /// the By Model cache share instead of counting it as a zero (#2027).
    pub async fn record(
        &self,
        session_id: &str,
        provider: &str,
        model: &str,
        token_count: i64,
        cost: f64,
        cache_read_tokens: Option<i64>,
    ) -> Result<()> {
        let sid = session_id.to_string();
        let prov = provider.to_string();
        let mdl = normalize_model_name(model);
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO usage_ledger (session_id, provider, model, token_count, cost, cache_read_tokens) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![sid, prov, mdl, token_count, cost, cache_read_tokens],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to record usage")?;

        Ok(())
    }

    /// Per-provider totals (#402): `(provider, tokens, cost)` ordered by
    /// cost. `since` filters by entry epoch; `filter` is a case-insensitive
    /// provider-name prefix. Empty provider renders as "unknown" (rows that
    /// predate the provider column whose session was already deleted).
    pub async fn by_provider(
        &self,
        since: Option<i64>,
        filter: Option<&str>,
    ) -> Result<Vec<(String, i64, f64)>> {
        let f = filter.map(|s| s.to_lowercase());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let sql = "SELECT CASE WHEN provider = '' THEN 'unknown' ELSE provider END AS p, \
                     COALESCE(SUM(token_count), 0), COALESCE(SUM(cost), 0.0) \
                     FROM usage_ledger WHERE (?1 IS NULL OR created_at >= ?1) \
                     AND (?2 IS NULL OR LOWER(provider) LIKE ?2 || '%') \
                     GROUP BY p ORDER BY 3 DESC";
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt
                    .query_map(params![since, f], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(rows)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to query provider breakdown")
    }

    /// Per-provider-per-model rows (#402): `(provider, model, tokens, cost)`
    /// ordered by cost, optionally filtered by provider prefix or exact
    /// model name.
    pub async fn by_provider_model(
        &self,
        since: Option<i64>,
        provider_filter: Option<&str>,
        model_filter: Option<&str>,
    ) -> Result<Vec<(String, String, i64, f64)>> {
        let pf = provider_filter.map(|s| s.to_lowercase());
        let mf = model_filter.map(normalize_model_name);
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let sql = "SELECT CASE WHEN provider = '' THEN 'unknown' ELSE provider END AS p, \
                     model, COALESCE(SUM(token_count), 0), COALESCE(SUM(cost), 0.0) \
                     FROM usage_ledger WHERE (?1 IS NULL OR created_at >= ?1) \
                     AND (?2 IS NULL OR LOWER(provider) LIKE ?2 || '%') \
                     AND (?3 IS NULL OR model = ?3) \
                     GROUP BY p, model ORDER BY 4 DESC";
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt
                    .query_map(params![since, pf, mf], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(rows)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to query provider/model breakdown")
    }

    /// Get all-time totals (tokens + cost)
    pub async fn totals(&self) -> Result<(i64, f64)> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(|conn| {
                conn.query_row(
                    "SELECT COALESCE(SUM(token_count), 0), COALESCE(SUM(cost), 0.0) FROM usage_ledger",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to query usage totals")
    }

    /// Get usage stats grouped by model.
    ///
    /// The grouping key comes from [`normalize_model_name`], in Rust, over the
    /// distinct raw names the ledger holds. The aggregate used to carry its own
    /// copy of that normalization in SQL, and the two had drifted (F-DB-04), so
    /// `/usage` could list one model twice: a row written as `k3` became
    /// `kimi-k3` on the way in and stayed `k3` on the way out. One
    /// implementation is the fix, and the only reason this can be done cheaply
    /// is that the ledger holds one row per call rather than one per token.
    ///
    /// Ordering is by cost, then by canonical name: two models that cost the same
    /// would otherwise come back in whatever order the map happened to produce,
    /// and a caller that renders the top row would show a different one run to
    /// run.
    pub async fn stats_by_model(&self) -> Result<Vec<ModelUsageStats>> {
        let raw: Vec<(String, i64, f64, i64)> = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT model, COALESCE(SUM(token_count), 0), \
                            COALESCE(SUM(cost), 0.0), COUNT(*) \
                     FROM usage_ledger WHERE model != '' GROUP BY model",
                )?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(rows)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to query usage by model")?;

        let mut merged: std::collections::HashMap<String, ModelUsageStats> =
            std::collections::HashMap::new();
        for (model, tokens, cost, count) in raw {
            let canonical = normalize_model_name(&model);
            let entry = merged.entry(canonical.clone()).or_insert(ModelUsageStats {
                model: canonical,
                total_tokens: 0,
                total_cost: 0.0,
                entry_count: 0,
            });
            entry.total_tokens += tokens;
            entry.total_cost += cost;
            entry.entry_count += count;
        }

        let mut stats: Vec<ModelUsageStats> = merged.into_values().collect();
        stats.sort_by(|a, b| {
            b.total_cost
                .total_cmp(&a.total_cost)
                .then_with(|| a.model.cmp(&b.model))
        });
        Ok(stats)
    }
}
