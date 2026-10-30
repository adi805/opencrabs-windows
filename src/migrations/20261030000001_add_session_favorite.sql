-- Favorite sessions: a user-pinned flag on sessions.
-- Favorites sort ahead of everything else in the session list; the flag is
-- user-controlled only and never set by background flows, so no backfill is
-- needed: every existing row reads as not-favorite. ALTER TABLE ADD COLUMN
-- with a constant default, so appended last per the list invariant; no heal
-- pass is needed.
ALTER TABLE sessions ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0;
