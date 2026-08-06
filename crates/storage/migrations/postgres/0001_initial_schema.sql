-- Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.
--
-- Initial Postgres schema. Same logical shape as the SQLite migration, but
-- uses native Postgres types (timestamptz, jsonb, real, integer, vector).
--
-- CRITICAL: All statements use IF NOT EXISTS so this migration is safe to
-- run against a SaaS database whose schema was already created by the
-- TypeScript activepieces API (TypeORM). In that mode every statement is a
-- no-op. On a fresh Postgres instance it creates the full schema.
--
-- The pgvector extension is required for the embedding column. If absent
-- the CREATE EXTENSION statement creates it; in environments where the
-- extension cannot be installed, the embedding column type falls back —
-- handled in code, not in this migration.

CREATE EXTENSION IF NOT EXISTS vector;

-- ────────────────────────────────────────────────────────────
-- clawdbot_memory_item
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS clawdbot_memory_item (
    id                      varchar(21) PRIMARY KEY NOT NULL,
    created                 timestamptz NOT NULL DEFAULT now(),
    updated                 timestamptz NOT NULL DEFAULT now(),
    "platformId"            varchar(21) NOT NULL,
    "projectId"             varchar(21),
    "locationId"            varchar(21),
    "chatbotId"             varchar(21),
    "chatIdentityId"        varchar(21),
    "sessionKey"            varchar,
    type                    varchar NOT NULL,
    content                 text NOT NULL,
    category                varchar,
    importance              real NOT NULL DEFAULT 5,
    source                  varchar,
    metadata                jsonb,
    embedding               vector(1536),
    scope                   varchar NOT NULL DEFAULT 'project',
    status                  varchar NOT NULL DEFAULT 'confirmed',
    confidence              real NOT NULL DEFAULT 1.0,
    impact                  varchar,
    "supersededById"        varchar(21),
    "confirmedByUserId"     varchar(21),
    "confirmedAt"           timestamptz,
    "negativeRatingCount"   integer NOT NULL DEFAULT 0,
    "validFrom"             timestamptz NOT NULL,
    "validTo"               timestamptz,
    "learnedAt"             timestamptz NOT NULL,
    "lastAccessedAt"        timestamptz NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_item_platform_id     ON clawdbot_memory_item ("platformId");
CREATE INDEX IF NOT EXISTS idx_memory_item_project_id      ON clawdbot_memory_item ("projectId");
CREATE INDEX IF NOT EXISTS idx_memory_item_project_type    ON clawdbot_memory_item ("projectId", type);
CREATE INDEX IF NOT EXISTS idx_memory_item_project_chatbot ON clawdbot_memory_item ("projectId", "chatbotId");
CREATE INDEX IF NOT EXISTS idx_memory_item_chat_identity   ON clawdbot_memory_item ("projectId", "chatIdentityId");
CREATE INDEX IF NOT EXISTS idx_memory_item_scope_status    ON clawdbot_memory_item ("platformId", scope, status);
CREATE INDEX IF NOT EXISTS idx_memory_item_platform_scope  ON clawdbot_memory_item ("platformId", "projectId", scope);
CREATE INDEX IF NOT EXISTS idx_memory_item_validity        ON clawdbot_memory_item ("platformId", "validFrom", "validTo");
CREATE INDEX IF NOT EXISTS idx_memory_item_learned_at      ON clawdbot_memory_item ("platformId", "learnedAt");
CREATE INDEX IF NOT EXISTS idx_memory_item_last_accessed   ON clawdbot_memory_item ("platformId", "lastAccessedAt");
CREATE INDEX IF NOT EXISTS idx_memory_item_embedding_hnsw  ON clawdbot_memory_item USING hnsw (embedding vector_cosine_ops);

-- ────────────────────────────────────────────────────────────
-- agent_memory_feedback
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS agent_memory_feedback (
    id                 varchar(21) PRIMARY KEY NOT NULL,
    created            timestamptz NOT NULL DEFAULT now(),
    updated            timestamptz NOT NULL DEFAULT now(),
    "memoryId"         varchar(21) NOT NULL,
    "responseId"       varchar,
    rating             varchar NOT NULL,
    comment            text,
    "createdByUserId"  varchar(21) NOT NULL,
    CONSTRAINT fk_memory_feedback_memory FOREIGN KEY ("memoryId")
        REFERENCES clawdbot_memory_item(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_memory_feedback_memory_id ON agent_memory_feedback ("memoryId");
CREATE INDEX IF NOT EXISTS idx_memory_feedback_user_id   ON agent_memory_feedback ("createdByUserId");

-- ────────────────────────────────────────────────────────────
-- memory_audit_event
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_audit_event (
    id              varchar(21) PRIMARY KEY NOT NULL,
    created         timestamptz NOT NULL DEFAULT now(),
    "platformId"    varchar(21) NOT NULL,
    "projectId"     varchar(21),
    actor           varchar(128) NOT NULL,
    "eventType"     varchar(32) NOT NULL,
    query           text,
    scope           varchar,
    "memoryIds"     text,
    "resultCount"   integer NOT NULL DEFAULT 0,
    metadata        jsonb
);

CREATE INDEX IF NOT EXISTS idx_memory_audit_platform_created ON memory_audit_event ("platformId", created DESC);
CREATE INDEX IF NOT EXISTS idx_memory_audit_project_created  ON memory_audit_event ("projectId", created DESC);
CREATE INDEX IF NOT EXISTS idx_memory_audit_actor            ON memory_audit_event ("platformId", actor, created DESC);
CREATE INDEX IF NOT EXISTS idx_memory_audit_event_type       ON memory_audit_event ("platformId", "eventType", created DESC);

-- ────────────────────────────────────────────────────────────
-- agent_memory_block
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS agent_memory_block (
    id                  varchar(21) PRIMARY KEY NOT NULL,
    created             timestamptz NOT NULL DEFAULT now(),
    updated             timestamptz NOT NULL DEFAULT now(),
    "platformId"        varchar(21) NOT NULL,
    "projectId"         varchar(21),
    "locationId"        varchar(21),
    "chatbotId"         varchar(21),
    "chatIdentityId"    varchar(21),
    "sessionKey"        varchar,
    scope               varchar NOT NULL,
    label               varchar(128) NOT NULL,
    value               text NOT NULL,
    "sizeLimitTokens"   integer NOT NULL DEFAULT 1000,
    importance          real NOT NULL DEFAULT 0.5,
    "lastEditedBy"      varchar,
    "lastEditedAt"      timestamptz,
    version             integer NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_memory_block_platform      ON agent_memory_block ("platformId", scope);
CREATE INDEX IF NOT EXISTS idx_memory_block_project       ON agent_memory_block ("projectId", scope);
CREATE INDEX IF NOT EXISTS idx_memory_block_chat_identity ON agent_memory_block ("chatIdentityId");

-- ────────────────────────────────────────────────────────────
-- memory_entity
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_entity (
    id                  varchar(21) PRIMARY KEY NOT NULL,
    created             timestamptz NOT NULL DEFAULT now(),
    updated             timestamptz NOT NULL DEFAULT now(),
    "platformId"        varchar(21) NOT NULL,
    "projectId"         varchar(21),
    "locationId"        varchar(21),
    "chatbotId"         varchar(21),
    "chatIdentityId"    varchar(21),
    scope               varchar NOT NULL DEFAULT 'project',
    type                varchar NOT NULL,
    "canonicalName"     varchar NOT NULL,
    aliases             text[] NOT NULL DEFAULT '{}',
    description         text,
    metadata            jsonb,
    "validFrom"         timestamptz NOT NULL DEFAULT now(),
    "validTo"           timestamptz,
    "supersededById"    varchar(21)
);

CREATE INDEX IF NOT EXISTS idx_memory_entity_platform  ON memory_entity ("platformId", scope);
CREATE INDEX IF NOT EXISTS idx_memory_entity_project   ON memory_entity ("projectId", type);
CREATE INDEX IF NOT EXISTS idx_memory_entity_canonical ON memory_entity ("platformId", type, "canonicalName");

-- ────────────────────────────────────────────────────────────
-- memory_edge
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_edge (
    id                  varchar(21) PRIMARY KEY NOT NULL,
    created             timestamptz NOT NULL DEFAULT now(),
    updated             timestamptz NOT NULL DEFAULT now(),
    "platformId"        varchar(21) NOT NULL,
    "projectId"         varchar(21),
    "locationId"        varchar(21),
    "chatbotId"         varchar(21),
    "chatIdentityId"    varchar(21),
    scope               varchar NOT NULL DEFAULT 'project',
    "subjectId"         varchar(21) NOT NULL,
    predicate           varchar NOT NULL,
    "objectId"          varchar(21),
    "objectLiteral"     text,
    weight              real NOT NULL DEFAULT 1.0,
    "sourceMemoryId"    varchar(21),
    metadata            jsonb,
    "validFrom"         timestamptz NOT NULL DEFAULT now(),
    "validTo"           timestamptz,
    CONSTRAINT fk_memory_edge_subject FOREIGN KEY ("subjectId")
        REFERENCES memory_entity(id) ON DELETE CASCADE,
    CONSTRAINT fk_memory_edge_object FOREIGN KEY ("objectId")
        REFERENCES memory_entity(id) ON DELETE SET NULL,
    CONSTRAINT fk_memory_edge_source_memory FOREIGN KEY ("sourceMemoryId")
        REFERENCES clawdbot_memory_item(id) ON DELETE SET NULL,
    CONSTRAINT chk_memory_edge_object_xor_literal CHECK (
        ("objectId" IS NOT NULL AND "objectLiteral" IS NULL) OR
        ("objectId" IS NULL AND "objectLiteral" IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS idx_memory_edge_subject  ON memory_edge ("subjectId", predicate);
CREATE INDEX IF NOT EXISTS idx_memory_edge_object   ON memory_edge ("objectId");
CREATE INDEX IF NOT EXISTS idx_memory_edge_platform ON memory_edge ("platformId", scope);

-- ────────────────────────────────────────────────────────────
-- clawdbot_contact
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS clawdbot_contact (
    id                    varchar(21) PRIMARY KEY NOT NULL,
    created               timestamptz NOT NULL DEFAULT now(),
    updated               timestamptz NOT NULL DEFAULT now(),
    "chatbotId"           varchar(21) NOT NULL,
    "projectId"           varchar(21),
    name                  varchar NOT NULL,
    phone                 varchar,
    email                 varchar,
    relationship          varchar,
    "isEmergency"         boolean NOT NULL DEFAULT false,
    priority              integer NOT NULL DEFAULT 0,
    notes                 text,
    metadata              jsonb,
    "externalIds"         jsonb,
    tags                  jsonb NOT NULL DEFAULT '[]',
    segment               varchar(50),
    "lifetimeValue"       decimal(12,2),
    "lastInteractionAt"   timestamptz,
    "chatIdentityId"      varchar(21)
);

CREATE INDEX IF NOT EXISTS idx_contact_chatbot ON clawdbot_contact ("chatbotId");
CREATE INDEX IF NOT EXISTS idx_contact_project ON clawdbot_contact ("projectId");
CREATE INDEX IF NOT EXISTS idx_contact_email   ON clawdbot_contact (email);

-- ────────────────────────────────────────────────────────────
-- clawdbot_contact_event
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS clawdbot_contact_event (
    id                         varchar(21) PRIMARY KEY NOT NULL,
    created                    timestamptz NOT NULL DEFAULT now(),
    updated                    timestamptz NOT NULL DEFAULT now(),
    "contactId"                varchar(21) NOT NULL,
    "projectId"                varchar(21) NOT NULL,
    "chatbotId"                varchar(21),
    "eventType"                varchar(64) NOT NULL,
    title                      varchar(500) NOT NULL,
    description                text,
    "eventData"                jsonb,
    "occurredAt"               timestamptz NOT NULL,
    source                     varchar(100),
    "engagementRuleId"         varchar(21),
    "engagementExecutionId"    varchar(21),
    CONSTRAINT fk_contact_event_contact FOREIGN KEY ("contactId")
        REFERENCES clawdbot_contact(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_contact_event_contact ON clawdbot_contact_event ("contactId", "occurredAt" DESC);
CREATE INDEX IF NOT EXISTS idx_contact_event_project ON clawdbot_contact_event ("projectId", "occurredAt" DESC);
CREATE INDEX IF NOT EXISTS idx_contact_event_type    ON clawdbot_contact_event ("projectId", "eventType", "occurredAt" DESC);

-- ────────────────────────────────────────────────────────────
-- agent_user_profile
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS agent_user_profile (
    id                            varchar(21) PRIMARY KEY NOT NULL,
    created                       timestamptz NOT NULL DEFAULT now(),
    updated                       timestamptz NOT NULL DEFAULT now(),
    "chatIdentityId"              varchar(21) NOT NULL,
    "projectId"                   varchar(21) NOT NULL,
    "platformId"                  varchar(21) NOT NULL,
    profile                       jsonb NOT NULL DEFAULT '{}',
    "profileVersion"              integer NOT NULL DEFAULT 1,
    "lastUpdatedFromSessionId"    varchar
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_user_profile_identity_project ON agent_user_profile ("chatIdentityId", "projectId");
CREATE INDEX IF NOT EXISTS idx_user_profile_chat_identity ON agent_user_profile ("chatIdentityId");
CREATE INDEX IF NOT EXISTS idx_user_profile_project       ON agent_user_profile ("projectId");
