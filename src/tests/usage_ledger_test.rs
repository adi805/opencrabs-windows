//! Usage Ledger Tests
//!
//! Tests for usage tracking, model name normalization, and stats aggregation.

use crate::db::Database;
use crate::db::repository::usage_ledger::{
    MODEL_FAMILIES, UsageLedgerRepository, normalize_model_name,
};

#[tokio::test]
async fn test_record_and_totals() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = UsageLedgerRepository::new(db.pool().clone());

    repo.record("s1", "prov", "sonnet-4-5", 100, 0.05, None)
        .await
        .unwrap();
    repo.record("s1", "prov", "sonnet-4-5", 200, 0.10, None)
        .await
        .unwrap();
    repo.record("s2", "prov", "opus-4-6", 500, 0.50, None)
        .await
        .unwrap();

    let (tokens, cost) = repo.totals().await.unwrap();
    assert_eq!(tokens, 800);
    assert!((cost - 0.65).abs() < 0.001);
}

#[tokio::test]
async fn test_stats_by_model() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = UsageLedgerRepository::new(db.pool().clone());

    repo.record("s1", "prov", "sonnet", 100, 0.05, None)
        .await
        .unwrap();
    repo.record("s2", "prov", "opus", 500, 0.50, None)
        .await
        .unwrap();
    repo.record("s3", "prov", "sonnet", 200, 0.10, None)
        .await
        .unwrap();

    let stats = repo.stats_by_model().await.unwrap();
    assert_eq!(stats.len(), 2);
    // normalize_model_name maps bare "opus" → "opus-4-6" at write time
    assert_eq!(stats[0].model, "opus-4-6");
    assert_eq!(stats[0].total_tokens, 500);
    assert_eq!(stats[1].model, "sonnet-4-6");
    assert_eq!(stats[1].total_tokens, 300);
}

#[tokio::test]
async fn test_stats_by_model_merges_claude_prefix() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = UsageLedgerRepository::new(db.pool().clone());

    repo.record("s1", "prov", "claude-opus-4-6", 1000, 1.0, None)
        .await
        .unwrap();
    repo.record("s2", "prov", "opus-4-6", 500, 0.50, None)
        .await
        .unwrap();
    repo.record("s3", "prov", "claude-sonnet-4-6", 200, 0.10, None)
        .await
        .unwrap();

    let stats = repo.stats_by_model().await.unwrap();
    assert_eq!(stats.len(), 2);
    assert_eq!(stats[0].model, "opus-4-6");
    assert_eq!(stats[0].total_tokens, 1500);
    assert_eq!(stats[1].model, "sonnet-4-6");
    assert_eq!(stats[1].total_tokens, 200);
}

#[test]
fn test_normalize_model_name() {
    // #697: a leaked "-@botname" attribution suffix is stripped so the same
    // model does not split into duplicate /usage rows.
    assert_eq!(
        normalize_model_name("qwen3.8-max-preview-@adicrabs_bot"),
        "qwen3.8-max-preview"
    );
    assert_eq!(
        normalize_model_name("qwen3.8-max-preview"),
        "qwen3.8-max-preview"
    );
    assert_eq!(normalize_model_name("opus@somebot"), "opus-4-6");

    // Claude normalization
    assert_eq!(normalize_model_name("claude-opus-4-6"), "opus-4-6");
    assert_eq!(normalize_model_name("claude-sonnet-4-6"), "sonnet-4-6");
    assert_eq!(normalize_model_name("opus"), "opus-4-6");
    assert_eq!(normalize_model_name("sonnet"), "sonnet-4-6");
    assert_eq!(normalize_model_name("haiku"), "haiku-4-5");
    assert_eq!(normalize_model_name("opus-4-6"), "opus-4-6");

    // Qwen 3.6 Plus normalization — all variants → canonical name
    assert_eq!(normalize_model_name("coder-model"), "qwen3.6-plus");
    assert_eq!(normalize_model_name("qwen-3.6-plus"), "qwen3.6-plus");
    assert_eq!(normalize_model_name("qwen3.6-plus"), "qwen3.6-plus");
    assert_eq!(normalize_model_name("qwen/qwen3.6-plus"), "qwen3.6-plus");
    assert_eq!(normalize_model_name("qwen3.6-plus:free"), "qwen3.6-plus");
    assert_eq!(normalize_model_name("qwen3.6-plus-free"), "qwen3.6-plus");
    assert_eq!(
        normalize_model_name("qwen-3.6-plus-thinking"),
        "qwen3.6-plus"
    );
    assert_eq!(
        normalize_model_name("qwen3.6-plus-thinking"),
        "qwen3.6-plus"
    );
    assert_eq!(
        normalize_model_name("qwen/qwen3.6-plus:free"),
        "qwen3.6-plus"
    );
    assert_eq!(
        normalize_model_name("opencode/qwen3.6-plus-free"),
        "qwen3.6-plus"
    );
    assert_eq!(normalize_model_name("qwen3.5-plus"), "qwen3.5-plus");
    assert_eq!(normalize_model_name("qwen-3.5-plus"), "qwen3.5-plus");

    // Provider prefix stripping
    assert_eq!(
        normalize_model_name("openrouter/elephant-model"),
        "elephant-model"
    );
    assert_eq!(
        normalize_model_name("opencode/qwen3.6-plus"),
        "qwen3.6-plus"
    );

    // Lowercase + normalization
    assert_eq!(normalize_model_name("MiniMax-M2.5"), "minimax-m2.5");
    assert_eq!(normalize_model_name("MiniMax-M2.7"), "minimax-m2.7");
    assert_eq!(normalize_model_name("minimax-m2.7"), "minimax-m2.7");
    assert_eq!(normalize_model_name("MiniMax-M3"), "minimax-m3");
    assert_eq!(normalize_model_name("minimax-m3"), "minimax-m3");
    assert_eq!(normalize_model_name("glm-5.1"), "glm-5.1");
    assert_eq!(normalize_model_name("glm-5-turbo"), "glm-5-turbo");
    assert_eq!(normalize_model_name("zhipu"), "glm-5-turbo");
    assert_eq!(normalize_model_name("zai"), "glm-5-turbo");
    assert_eq!(normalize_model_name("mimo-v2-omni-free"), "mimo-v2-omni");
    assert_eq!(normalize_model_name("mimo-v2-pro-free"), "mimo-v2-pro");
    assert_eq!(normalize_model_name("kimi-k2.5"), "kimi-k2.5");
    // Kimi K3 live model id "k3" folds to the canonical "kimi-k3" pricing key
    assert_eq!(normalize_model_name("k3"), "kimi-k3");
    assert_eq!(normalize_model_name("kimi-k3"), "kimi-k3");
    // Everything lowercased
    assert_eq!(normalize_model_name("GPT-5-mini"), "gpt-5-mini");
}

#[tokio::test]
async fn provider_breakdowns_group_filter_and_handle_unknown() {
    let db = crate::db::Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = crate::db::repository::UsageLedgerRepository::new(db.pool().clone());

    repo.record(
        "s1",
        "custom.opencode-go",
        "deepseek-v4-flash",
        100,
        1.0,
        None,
    )
    .await
    .unwrap();
    repo.record(
        "s2",
        "custom.nvidia-ds4flash",
        "deepseek-v4-flash",
        200,
        2.0,
        None,
    )
    .await
    .unwrap();
    repo.record("s3", "custom.opencode-go", "mimo-v2.5", 50, 0.5, None)
        .await
        .unwrap();
    repo.record("s4", "", "mystery-model", 10, 0.1, None)
        .await
        .unwrap();

    // Per-provider totals, ordered by cost; empty provider -> "unknown".
    let rows = repo.by_provider(None, None).await.unwrap();
    assert_eq!(rows.len(), 3, "rows: {rows:?}");
    assert_eq!(rows[0].0, "custom.nvidia-ds4flash");
    assert_eq!(rows[0].1, 200);
    assert!(rows.iter().any(|r| r.0 == "unknown" && r.1 == 10));

    // Prefix filter is case-insensitive.
    let nvidia = repo.by_provider(None, Some("CUSTOM.NVIDIA")).await.unwrap();
    assert_eq!(nvidia.len(), 1);
    assert_eq!(nvidia[0].2, 2.0);

    // Same model across providers splits correctly.
    let per = repo
        .by_provider_model(None, None, Some("deepseek-v4-flash"))
        .await
        .unwrap();
    assert_eq!(per.len(), 2, "per: {per:?}");
    assert!(per.iter().all(|r| r.1 == "deepseek-v4-flash"));

    // Period filter: nothing recorded before epoch 0 boundary in future.
    let future = repo
        .by_provider(Some(chrono::Utc::now().timestamp() + 3600), None)
        .await
        .unwrap();
    assert!(future.is_empty());
}

// ── one normalizer, shared by the writer and the reader (F-DB-04) ─────

/// Every alias in the table normalizes to its canonical name, and a canonical
/// name is already normalized. This is the parity the drift broke: the reader
/// carried its own copy of this mapping in SQL, and the two disagreed on
/// `k3`/`kimi-k3` and on `zai`.
#[test]
fn every_family_alias_normalizes_to_its_canonical_name() {
    for (canonical, aliases) in MODEL_FAMILIES {
        for alias in *aliases {
            assert_eq!(
                normalize_model_name(alias),
                *canonical,
                "{alias} must normalize to {canonical}"
            );
        }
        // Idempotent: feeding the canonical name back changes nothing, or a
        // reader that normalizes twice would still split a model in two.
        assert_eq!(
            normalize_model_name(canonical),
            *canonical,
            "{canonical} must be stable under a second normalization"
        );
    }
}

/// A family's canonical name must not also appear as an alias of another
/// family, or the table's order would decide the answer.
#[test]
fn no_canonical_name_is_another_familys_alias() {
    let canonicals: Vec<&str> = MODEL_FAMILIES.iter().map(|(c, _)| *c).collect();
    for (canonical, aliases) in MODEL_FAMILIES {
        for alias in *aliases {
            if alias == canonical {
                continue;
            }
            assert!(
                !canonicals.contains(alias),
                "{alias} is an alias in {canonical} AND a canonical name, so which \
                 family wins would depend on table order"
            );
        }
    }
}

/// The exact drift, end to end: a row stored under `k3` (which is what the
/// ledger's backfill migration does, since it copies `sessions.model` raw) has
/// to land in the same bucket as a row recorded as `kimi-k3`, not beside it.
#[tokio::test]
async fn a_raw_legacy_spelling_groups_with_its_canonical_name() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = UsageLedgerRepository::new(db.pool().clone());

    // The writer normalizes, so this row is stored as kimi-k3.
    repo.record("s1", "prov", "kimi-k3", 100, 0.10, None)
        .await
        .unwrap();

    // A backfilled row keeps whatever the session held.
    //
    // Scoped on purpose. The in-memory pool holds exactly ONE connection
    // (`connect_in_memory` caps it at `max_size(1)`, deliberately, so tests
    // cannot hit cross-connection SQLite locking), so holding this one across
    // the `stats_by_model()` call below would leave that call waiting for a slot
    // that can never free: it fails on the 60s acquire timeout, and it fails on
    // the pool rather than on anything this test is about. Dropping it at the
    // end of the block returns the slot first.
    {
        let conn = db.pool().get().await.expect("connection");
        conn.interact(|conn| {
            conn.execute(
                "INSERT INTO usage_ledger (session_id, provider, model, token_count, cost) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["s0", "prov", "k3", 300, 0.30],
            )
        })
        .await
        .expect("interact")
        .expect("raw insert");
    }

    let stats = repo.stats_by_model().await.unwrap();
    assert_eq!(stats.len(), 1, "k3 and kimi-k3 are one model: {stats:?}");
    assert_eq!(stats[0].model, "kimi-k3");
    assert_eq!(stats[0].total_tokens, 400);
    assert_eq!(stats[0].entry_count, 2);
}

/// Two models at the same cost must come back in a stable order, or a caller
/// rendering the top row shows a different one run to run. The previous SQL
/// aggregate ordered by cost alone.
#[tokio::test]
async fn equal_cost_models_have_a_deterministic_order() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = UsageLedgerRepository::new(db.pool().clone());

    // Same cost, opposite insertion order from the alphabetical one.
    repo.record("s1", "prov", "glm-5.1", 10, 0.25, None)
        .await
        .unwrap();
    repo.record("s1", "prov", "opus", 10, 0.25, None)
        .await
        .unwrap();

    let first = repo.stats_by_model().await.unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(
        first[0].model, "glm-5.1",
        "a cost tie breaks by canonical name: {first:?}"
    );
    assert_eq!(first[1].model, "opus-4-6");

    // And it is stable across a second call rather than depending on map order.
    let second = repo.stats_by_model().await.unwrap();
    assert_eq!(
        first.iter().map(|s| &s.model).collect::<Vec<_>>(),
        second.iter().map(|s| &s.model).collect::<Vec<_>>()
    );
}
