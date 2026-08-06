-- Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.
--
-- Postgres mirror of the local sync foundation. Used when the engine runs in
-- a Postgres-backed mode (self-hosted SaaS install). The hosted SaaS uses
-- TypeORM-managed schema and does NOT run these migrations — it has its own.

ALTER TABLE clawdbot_memory_item
    ADD COLUMN IF NOT EXISTS sync_status TEXT NOT NULL DEFAULT 'pending';

CREATE INDEX IF NOT EXISTS idx_memory_item_sync_status
    ON clawdbot_memory_item (sync_status, updated);

CREATE TABLE IF NOT EXISTS sync_state (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS project_bindings (
    cwd          TEXT PRIMARY KEY NOT NULL,
    platform_id  TEXT NOT NULL,
    project_id   TEXT NOT NULL,
    created      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_project_bindings_project
    ON project_bindings (platform_id, project_id);
