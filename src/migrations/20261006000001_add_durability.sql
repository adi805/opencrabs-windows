-- Milestone 2: durability foundation (Pi Durable invariants).
--
-- Four artifacts, one per requirement. Everything here is additive: new
-- tables use `IF NOT EXISTS` and the only ALTERs are nullable column adds,
-- so a 648 MB live database with 220k legacy `tool_executions` rows pays a
-- schema change and no backfill.

-- FR-002: the turn journal. A turn is one durability unit: the row is opened
-- when the turn starts and settled in the SAME transaction that writes the
-- assistant message, its tool executions and its usage. A process kill
-- mid-turn therefore leaves either a complete turn or none, never a torn
-- one. `state` is one of `running | committed | interrupted | failed`;
-- boot scans for `running` rows and marks them `interrupted`.
CREATE TABLE IF NOT EXISTS turns (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'running',
    started_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    committed_at INTEGER,
    error TEXT
);
CREATE INDEX IF NOT EXISTS idx_turns_session_state ON turns(session_id, state);

-- FR-003: the effect ledger. A tool call is an external effect: the intent is
-- recorded BEFORE the effect runs, the settlement AFTER, and every effect
-- carries an idempotency key so a resumed turn can tell "already landed" from
-- "not yet" instead of replaying it. Legacy rows keep NULLs by design: the
-- columns are only populated going forward, so no backfill is needed.
ALTER TABLE tool_executions ADD COLUMN turn_id TEXT;
ALTER TABLE tool_executions ADD COLUMN args_hash TEXT;
ALTER TABLE tool_executions ADD COLUMN result_preview TEXT;
ALTER TABLE tool_executions ADD COLUMN attempt INTEGER NOT NULL DEFAULT 1;
ALTER TABLE tool_executions ADD COLUMN effect_key TEXT;
ALTER TABLE tool_executions ADD COLUMN committed_at INTEGER;
CREATE INDEX IF NOT EXISTS idx_tool_executions_effect_key ON tool_executions(effect_key);
CREATE INDEX IF NOT EXISTS idx_tool_executions_turn ON tool_executions(turn_id);

-- FR-004: idempotent submissions. A submit carrying a `requestId` that is
-- already known returns the existing submission instead of starting a second
-- run. The mobile client retries on network blips and re-attaches after a
-- dropped connection, so without this a retry duplicates the user's input.
CREATE TABLE IF NOT EXISTS submissions (
    request_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'queued',
    message_id TEXT,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_submissions_session ON submissions(session_id, state);

-- FR-005: provider session identity. The provider-side session id is
-- persisted so prompt cache and session affinity survive reopen, retry,
-- reset and model changes. Analog of Pi Durable's `pi.provider`. It changes
-- only when a session is forked.
CREATE TABLE IF NOT EXISTS session_identity (
    session_id TEXT PRIMARY KEY,
    provider_session_id TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);
