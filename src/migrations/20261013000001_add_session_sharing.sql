-- #217: durable shared/personal classification on sessions.
--
-- The classification lived only in a process-local set, which starts empty.
-- `is_session_shared` returned false for an absent entry, and false means
-- "not shared", so an unclassified session was treated as private and the
-- personal-memory gates stayed open. Boot recovery dispatches interrupted
-- channel turns straight to the resume path, bypassing ingress, so a resumed
-- guild turn could prepare context before any inbound message re-marked it.
--
-- NULL is the honest third state: this session has never been classified.
-- It is NOT backfilled to 'personal' for channel-bound rows, because that is
-- exactly the fail-open answer the issue is about. Channel sessions re-mark
-- on their next inbound message, and until then an unknown classification
-- blocks personal content.
--
-- Local (non-channel) sessions are different: a NULL channel_chat_key means
-- the row was never bound to a chat, so it is the owner's own session and
-- 'personal' is the correct durable answer rather than a guess.
ALTER TABLE sessions ADD COLUMN sharing TEXT;

UPDATE sessions
SET sharing = 'personal'
WHERE sharing IS NULL
  AND channel_chat_key IS NULL;
