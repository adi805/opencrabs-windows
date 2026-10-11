-- Discord card state: the tool-group and pager stores survive a restart.
--
-- Discord's collapsible tool group (#380) and its paged long answers (FR-009)
-- live only in `DiscordState`'s in-process maps. The plan card already has a
-- durable backing (`plan_cards`, #104/#809) because losing it stranded a stale
-- checklist in the channel. These two stores have the same defect with a
-- shorter fuse: after any restart the card stays on screen WITH its button,
-- and the press answers "This status bubble is too old to expand" or "That
-- pager aged out", because the message id is no longer in the map.
--
-- One row per card, keyed by the message id its button carries (a snowflake,
-- globally unique, so no channel column is needed to address it). `kind` is
-- `tool_group` or `long_answer`; `state` is the JSON snapshot the owning store
-- knows how to rebuild from. Rows are deleted in lockstep with the in-process
-- eviction, so the table is bounded by the same caps the maps are and never
-- needs a time-based prune.
CREATE TABLE IF NOT EXISTS discord_card_state (
    message_id INTEGER PRIMARY KEY NOT NULL,
    kind       TEXT NOT NULL,
    state      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
