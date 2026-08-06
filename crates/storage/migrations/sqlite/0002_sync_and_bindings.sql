-- Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.
--
-- Sync foundation: per-item sync status, key/value sync state for cursors and
-- counters, and per-machine project bindings (cwd → SaaS platform+project).
--
-- These tables are local-only. The bindings table is intentionally NOT synced
-- to the SaaS — cwd paths are per-machine. The sync_state table is also
-- local-only — it tracks where this device is in the pull cursor and when it
-- last successfully pushed.

-- ────────────────────────────────────────────────────────────
-- clawdbot_memory_item.sync_status — per-row sync state
--   'pending'  → row needs to be pushed (default for new + modified rows)
--   'synced'   → row has been pushed and acknowledged
--   'conflict' → server rejected the push; needs manual resolution
-- ────────────────────────────────────────────────────────────
ALTER TABLE clawdbot_memory_item
    ADD COLUMN sync_status TEXT NOT NULL DEFAULT 'pending';

CREATE INDEX IF NOT EXISTS idx_memory_item_sync_status
    ON clawdbot_memory_item (sync_status, updated);

-- ────────────────────────────────────────────────────────────
-- sync_state — single-row-style key/value store for sync bookkeeping.
-- Used for:
--   'pull_cursor'     → opaque cursor returned by the SaaS pull endpoint
--   'last_push_at'    → ISO timestamp of the most recent successful push
--   'last_pull_at'    → ISO timestamp of the most recent successful pull
--   'plan_tier'       → 'free' | 'paid' (cached from the most recent token
--                       validation; the source of truth is still the SaaS)
--   'platform_id'     → resolved SaaS platform id (mirror of config.toml for
--                       quick reads without re-parsing config)
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS sync_state (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- ────────────────────────────────────────────────────────────
-- project_bindings — local mapping from working-directory path to
-- SaaS platform + project identity. Per-machine; never synced.
--
-- Lookup precedence is exact-match on cwd. Resolver may walk up the dir
-- tree at call time (handled in the bindings module, not at the SQL layer)
-- so worktrees / subdirs inherit a parent binding.
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS project_bindings (
    cwd          TEXT PRIMARY KEY NOT NULL,
    platform_id  TEXT NOT NULL,
    project_id   TEXT NOT NULL,
    created      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_project_bindings_project
    ON project_bindings (platform_id, project_id);
