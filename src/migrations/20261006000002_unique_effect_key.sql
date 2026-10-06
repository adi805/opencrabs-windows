-- FR-003 follow-up: the effect ledger's idempotency key has to be enforced by
-- the storage layer, not by the caller's hope.
--
-- `record_intent` writes with `INSERT OR IGNORE`, which only ignores a
-- CONSTRAINT violation. The index added by 20261006000001_add_durability.sql
-- was plain, not unique, so there was nothing to conflict on: replaying the
-- same provider tool-use id after a resume opened a second ledger row for one
-- effect. `pending_for_turn` then reported the call twice and a settle could
-- land on the replay's row, which is the exact failure the key exists to
-- prevent. The replay case in `durability_effect_ledger_test` caught it in CI.
--
-- Ordering matters and is not reorderable:
--   1. collapse duplicates, keeping the earliest row per key. Once the index
--      exists the first INSERT wins the conflict, so the survivor here is the
--      row the writer would have kept; a clean image deletes nothing. Only
--      non-NULL keys can be duplicated: the analytics writers leave the column
--      NULL, and a unique index skips NULLs.
--   2. drop the plain index (strictly subsumed: every read compares against a
--      non-NULL key) and recreate it UNIQUE and partial on non-NULL keys, so
--      every legacy analytics row stays out of the constraint.
--   3. nothing outside the ledger reads `effect_key` yet, so no reader needs a
--      matching change.
DELETE FROM tool_executions
WHERE effect_key IS NOT NULL
  AND rowid NOT IN (
    SELECT MIN(rowid)
    FROM tool_executions
    WHERE effect_key IS NOT NULL
    GROUP BY effect_key
  );

DROP INDEX IF EXISTS idx_tool_executions_effect_key;

CREATE UNIQUE INDEX IF NOT EXISTS idx_tool_executions_effect_key
  ON tool_executions(effect_key)
  WHERE effect_key IS NOT NULL;
