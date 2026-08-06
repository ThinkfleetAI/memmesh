-- Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.
--
-- Initial SQLite schema. Mirrors the SaaS Postgres schema column-for-column.
-- Type mapping:
--   varchar         -> TEXT
--   timestamptz     -> TEXT (ISO 8601, UTC)
--   real / decimal  -> REAL
--   integer         -> INTEGER
--   boolean         -> INTEGER (0/1)
--   jsonb / json    -> TEXT (JSON string)
--   vector(N)       -> BLOB (raw f32 bytes; sqlite-vec virtual table built on
--                     top, not in this migration)
--   text[]          -> TEXT (JSON array string)
--
-- FK to tables this engine doesn't own (chatbot, location, project, user,
-- platform) are omitted intentionally — the desktop runtime doesn't have
-- those tables, and the SaaS runtime owns them via TypeORM.

-- ────────────────────────────────────────────────────────────
-- clawdbot_memory_item — primary memory fact table
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS clawdbot_memory_item (
    id                      TEXT PRIMARY KEY NOT NULL,
    created                 TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated                 TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    platformId              TEXT NOT NULL,
    projectId               TEXT,
    locationId              TEXT,
    chatbotId               TEXT,
    chatIdentityId          TEXT,
    sessionKey              TEXT,
    type                    TEXT NOT NULL,
    content                 TEXT NOT NULL,
    category                TEXT,
    importance              REAL NOT NULL DEFAULT 5,
    source                  TEXT,
    metadata                TEXT,
    embedding               BLOB,
    scope                   TEXT NOT NULL DEFAULT 'project',
    status                  TEXT NOT NULL DEFAULT 'confirmed',
    confidence              REAL NOT NULL DEFAULT 1.0,
    impact                  TEXT,
    supersededById          TEXT,
    confirmedByUserId       TEXT,
    confirmedAt             TEXT,
    negativeRatingCount     INTEGER NOT NULL DEFAULT 0,
    validFrom               TEXT NOT NULL,
    validTo                 TEXT,
    learnedAt               TEXT NOT NULL,
    lastAccessedAt          TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_item_platform_id        ON clawdbot_memory_item (platformId);
CREATE INDEX IF NOT EXISTS idx_memory_item_project_id         ON clawdbot_memory_item (projectId);
CREATE INDEX IF NOT EXISTS idx_memory_item_project_type       ON clawdbot_memory_item (projectId, type);
CREATE INDEX IF NOT EXISTS idx_memory_item_project_chatbot    ON clawdbot_memory_item (projectId, chatbotId);
CREATE INDEX IF NOT EXISTS idx_memory_item_chat_identity      ON clawdbot_memory_item (projectId, chatIdentityId);
CREATE INDEX IF NOT EXISTS idx_memory_item_scope_status       ON clawdbot_memory_item (platformId, scope, status);
CREATE INDEX IF NOT EXISTS idx_memory_item_platform_scope     ON clawdbot_memory_item (platformId, projectId, scope);
CREATE INDEX IF NOT EXISTS idx_memory_item_validity           ON clawdbot_memory_item (platformId, validFrom, validTo);
CREATE INDEX IF NOT EXISTS idx_memory_item_learned_at         ON clawdbot_memory_item (platformId, learnedAt);
CREATE INDEX IF NOT EXISTS idx_memory_item_last_accessed      ON clawdbot_memory_item (platformId, lastAccessedAt);

-- ────────────────────────────────────────────────────────────
-- agent_memory_feedback — positive / negative ratings on memory items
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS agent_memory_feedback (
    id                 TEXT PRIMARY KEY NOT NULL,
    created            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    memoryId           TEXT NOT NULL,
    responseId         TEXT,
    rating             TEXT NOT NULL,
    comment            TEXT,
    createdByUserId    TEXT NOT NULL,
    FOREIGN KEY (memoryId) REFERENCES clawdbot_memory_item(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_memory_feedback_memory_id ON agent_memory_feedback (memoryId);
CREATE INDEX IF NOT EXISTS idx_memory_feedback_user_id   ON agent_memory_feedback (createdByUserId);

-- ────────────────────────────────────────────────────────────
-- memory_audit_event — append-only access audit log
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_audit_event (
    id              TEXT PRIMARY KEY NOT NULL,
    created         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    platformId      TEXT NOT NULL,
    projectId       TEXT,
    actor           TEXT NOT NULL,
    eventType       TEXT NOT NULL,
    query           TEXT,
    scope           TEXT,
    memoryIds       TEXT,
    resultCount     INTEGER NOT NULL DEFAULT 0,
    metadata        TEXT
);

CREATE INDEX IF NOT EXISTS idx_memory_audit_platform_created ON memory_audit_event (platformId, created DESC);
CREATE INDEX IF NOT EXISTS idx_memory_audit_project_created  ON memory_audit_event (projectId, created DESC);
CREATE INDEX IF NOT EXISTS idx_memory_audit_actor            ON memory_audit_event (platformId, actor, created DESC);
CREATE INDEX IF NOT EXISTS idx_memory_audit_event_type       ON memory_audit_event (platformId, eventType, created DESC);

-- ────────────────────────────────────────────────────────────
-- agent_memory_block — labeled scratchpad memory
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS agent_memory_block (
    id                 TEXT PRIMARY KEY NOT NULL,
    created            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    platformId         TEXT NOT NULL,
    projectId          TEXT,
    locationId         TEXT,
    chatbotId          TEXT,
    chatIdentityId     TEXT,
    sessionKey         TEXT,
    scope              TEXT NOT NULL,
    label              TEXT NOT NULL,
    value              TEXT NOT NULL,
    sizeLimitTokens    INTEGER NOT NULL DEFAULT 1000,
    importance         REAL NOT NULL DEFAULT 0.5,
    lastEditedBy       TEXT,
    lastEditedAt       TEXT,
    version            INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_memory_block_platform     ON agent_memory_block (platformId, scope);
CREATE INDEX IF NOT EXISTS idx_memory_block_project      ON agent_memory_block (projectId, scope);
CREATE INDEX IF NOT EXISTS idx_memory_block_chat_identity ON agent_memory_block (chatIdentityId);

-- ────────────────────────────────────────────────────────────
-- memory_entity — knowledge-graph typed entities
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_entity (
    id                 TEXT PRIMARY KEY NOT NULL,
    created            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    platformId         TEXT NOT NULL,
    projectId          TEXT,
    locationId         TEXT,
    chatbotId          TEXT,
    chatIdentityId     TEXT,
    scope              TEXT NOT NULL DEFAULT 'project',
    type               TEXT NOT NULL,
    canonicalName      TEXT NOT NULL,
    aliases            TEXT NOT NULL DEFAULT '[]',
    description        TEXT,
    metadata           TEXT,
    validFrom          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    validTo            TEXT,
    supersededById     TEXT
);

CREATE INDEX IF NOT EXISTS idx_memory_entity_platform   ON memory_entity (platformId, scope);
CREATE INDEX IF NOT EXISTS idx_memory_entity_project    ON memory_entity (projectId, type);
CREATE INDEX IF NOT EXISTS idx_memory_entity_canonical  ON memory_entity (platformId, type, canonicalName);

-- ────────────────────────────────────────────────────────────
-- memory_edge — knowledge-graph relationships
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_edge (
    id                 TEXT PRIMARY KEY NOT NULL,
    created            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    platformId         TEXT NOT NULL,
    projectId          TEXT,
    locationId         TEXT,
    chatbotId          TEXT,
    chatIdentityId     TEXT,
    scope              TEXT NOT NULL DEFAULT 'project',
    subjectId          TEXT NOT NULL,
    predicate          TEXT NOT NULL,
    objectId           TEXT,
    objectLiteral      TEXT,
    weight             REAL NOT NULL DEFAULT 1.0,
    sourceMemoryId     TEXT,
    metadata           TEXT,
    validFrom          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    validTo            TEXT,
    FOREIGN KEY (subjectId)      REFERENCES memory_entity(id)        ON DELETE CASCADE,
    FOREIGN KEY (objectId)       REFERENCES memory_entity(id)        ON DELETE SET NULL,
    FOREIGN KEY (sourceMemoryId) REFERENCES clawdbot_memory_item(id) ON DELETE SET NULL,
    CHECK ((objectId IS NOT NULL AND objectLiteral IS NULL) OR
           (objectId IS NULL AND objectLiteral IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS idx_memory_edge_subject    ON memory_edge (subjectId, predicate);
CREATE INDEX IF NOT EXISTS idx_memory_edge_object     ON memory_edge (objectId);
CREATE INDEX IF NOT EXISTS idx_memory_edge_platform   ON memory_edge (platformId, scope);

-- ────────────────────────────────────────────────────────────
-- clawdbot_contact — contact records used by lattice patterns
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS clawdbot_contact (
    id                  TEXT PRIMARY KEY NOT NULL,
    created             TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated             TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    chatbotId           TEXT NOT NULL,
    projectId           TEXT,
    name                TEXT NOT NULL,
    phone               TEXT,
    email               TEXT,
    relationship        TEXT,
    isEmergency         INTEGER NOT NULL DEFAULT 0,
    priority            INTEGER NOT NULL DEFAULT 0,
    notes               TEXT,
    metadata            TEXT,
    externalIds         TEXT,
    tags                TEXT NOT NULL DEFAULT '[]',
    segment             TEXT,
    lifetimeValue       REAL,
    lastInteractionAt   TEXT,
    chatIdentityId      TEXT
);

CREATE INDEX IF NOT EXISTS idx_contact_chatbot ON clawdbot_contact (chatbotId);
CREATE INDEX IF NOT EXISTS idx_contact_project ON clawdbot_contact (projectId);
CREATE INDEX IF NOT EXISTS idx_contact_email   ON clawdbot_contact (email);

-- ────────────────────────────────────────────────────────────
-- clawdbot_contact_event — contact interaction events (lattice input)
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS clawdbot_contact_event (
    id                       TEXT PRIMARY KEY NOT NULL,
    created                  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated                  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    contactId                TEXT NOT NULL,
    projectId                TEXT NOT NULL,
    chatbotId                TEXT,
    eventType                TEXT NOT NULL,
    title                    TEXT NOT NULL,
    description              TEXT,
    eventData                TEXT,
    occurredAt               TEXT NOT NULL,
    source                   TEXT,
    engagementRuleId         TEXT,
    engagementExecutionId    TEXT,
    FOREIGN KEY (contactId) REFERENCES clawdbot_contact(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_contact_event_contact   ON clawdbot_contact_event (contactId, occurredAt DESC);
CREATE INDEX IF NOT EXISTS idx_contact_event_project   ON clawdbot_contact_event (projectId, occurredAt DESC);
CREATE INDEX IF NOT EXISTS idx_contact_event_type      ON clawdbot_contact_event (projectId, eventType, occurredAt DESC);

-- ────────────────────────────────────────────────────────────
-- agent_user_profile — per-identity behavior profile (lattice output)
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS agent_user_profile (
    id                             TEXT PRIMARY KEY NOT NULL,
    created                        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated                        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    chatIdentityId                 TEXT NOT NULL,
    projectId                      TEXT NOT NULL,
    platformId                     TEXT NOT NULL,
    profile                        TEXT NOT NULL DEFAULT '{}',
    profileVersion                 INTEGER NOT NULL DEFAULT 1,
    lastUpdatedFromSessionId       TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_user_profile_identity_project ON agent_user_profile (chatIdentityId, projectId);
CREATE INDEX IF NOT EXISTS idx_user_profile_chat_identity ON agent_user_profile (chatIdentityId);
CREATE INDEX IF NOT EXISTS idx_user_profile_project       ON agent_user_profile (projectId);
