-- Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.
--
-- Typed-attribute layer: the keystone that lets the engine reason over
-- structured/numeric data (credit scores, sensor readings, balances, ...)
-- instead of burying it in opaque metadata JSON.
--
-- Four tables:
--   memory_attribute_registry     — per (platform, project) field definitions; drives validation
--   memory_typed_observation      — high-volume measurement store; HASH-partitioned by platformId
--   memory_attribute_accumulator  — per (subject, attribute) running stats, O(1) incremental update
--   memory_ingest_ledger          — exactly-once guard for accumulation under at-least-once delivery
--
-- All statements use IF NOT EXISTS so the migration is a no-op against a
-- database that already has these tables (same convention as 0001).
-- Column names are quoted camelCase to match the TypeORM/SaaS schema.

-- ────────────────────────────────────────────────────────────
-- memory_attribute_registry
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_attribute_registry (
    id              varchar(21) PRIMARY KEY NOT NULL,
    created         timestamptz NOT NULL DEFAULT now(),
    updated         timestamptz NOT NULL DEFAULT now(),
    "platformId"    varchar(21) NOT NULL,
    "projectId"     varchar(21),
    "attributeKey"  varchar(128) NOT NULL,
    -- one of: numeric | categorical | temporal | boolean
    "dataType"      varchar(16) NOT NULL,
    unit            varchar(32),
    "minValid"      double precision,
    "maxValid"      double precision,
    required        boolean NOT NULL DEFAULT false,
    metadata        jsonb
);

-- Uniqueness on (platform, project, key). projectId is nullable, so COALESCE
-- to an empty sentinel — otherwise NULL projectId rows would never collide.
CREATE UNIQUE INDEX IF NOT EXISTS idx_attr_registry_key
    ON memory_attribute_registry ("platformId", COALESCE("projectId", ''), "attributeKey");

-- ────────────────────────────────────────────────────────────
-- memory_typed_observation  (HASH-partitioned by platformId)
-- ────────────────────────────────────────────────────────────
-- The partition key must be part of the primary key, hence (id, platformId).
-- id is a globally-unique nanoid on its own; platformId just satisfies the
-- partitioning constraint and keeps each tenant's rows co-located.
CREATE TABLE IF NOT EXISTS memory_typed_observation (
    id                  varchar(21) NOT NULL,
    "platformId"        varchar(21) NOT NULL,
    "projectId"         varchar(21),
    "subjectKind"       varchar(64) NOT NULL,
    "subjectExternalId" varchar(128) NOT NULL,
    "attributeKey"      varchar(128) NOT NULL,
    "valueNumeric"      double precision,
    "valueText"         text,
    "valueBool"         boolean,
    "valueTs"           timestamptz,
    "observedAt"        timestamptz NOT NULL,
    source              varchar(100),
    trust               real NOT NULL DEFAULT 1.0,
    "qualityScore"      real NOT NULL DEFAULT 1.0,
    -- accepted | quarantined
    status              varchar(16) NOT NULL DEFAULT 'accepted',
    "ingestedAt"        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id, "platformId")
) PARTITION BY HASH ("platformId");

-- 16 hash partitions (tunable). Spreads a single tenant's high-volume writes
-- and lets the planner prune. Increase the modulus in a follow-up migration if
-- one tenant dwarfs the rest; range sub-partitioning by observedAt is deferred.
CREATE TABLE IF NOT EXISTS memory_typed_observation_p00 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 0);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p01 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 1);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p02 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 2);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p03 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 3);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p04 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 4);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p05 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 5);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p06 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 6);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p07 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 7);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p08 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 8);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p09 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 9);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p10 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 10);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p11 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 11);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p12 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 12);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p13 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 13);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p14 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 14);
CREATE TABLE IF NOT EXISTS memory_typed_observation_p15 PARTITION OF memory_typed_observation FOR VALUES WITH (MODULUS 16, REMAINDER 15);

-- Indexes declared on the parent propagate to every partition (PG11+).
-- Time-series + per-subject scans:
CREATE INDEX IF NOT EXISTS idx_typed_obs_subject
    ON memory_typed_observation ("platformId", "subjectKind", "subjectExternalId", "attributeKey", "observedAt" DESC);
-- Cross-subject numeric range queries ("all subjects with attr in [a,b]"):
CREATE INDEX IF NOT EXISTS idx_typed_obs_range
    ON memory_typed_observation ("platformId", "projectId", "attributeKey", "valueNumeric");

-- ────────────────────────────────────────────────────────────
-- memory_attribute_accumulator  (O(1) incremental running stats)
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_attribute_accumulator (
    "platformId"        varchar(21) NOT NULL,
    "subjectKind"       varchar(64) NOT NULL,
    "subjectExternalId" varchar(128) NOT NULL,
    "attributeKey"      varchar(128) NOT NULL,
    "projectId"         varchar(21),
    count               bigint NOT NULL DEFAULT 0,
    sum                 double precision NOT NULL DEFAULT 0,
    "sumSq"             double precision NOT NULL DEFAULT 0,
    "minVal"            double precision,
    "maxVal"            double precision,
    "lastVal"           double precision,
    "lastObservedAt"    timestamptz,
    cumulative          double precision NOT NULL DEFAULT 0,
    ewma                double precision,
    "ewmaVar"           double precision,
    updated             timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY ("platformId", "subjectKind", "subjectExternalId", "attributeKey")
);

-- ────────────────────────────────────────────────────────────
-- memory_ingest_ledger  (exactly-once accumulation guard)
-- ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_ingest_ledger (
    "observationId" varchar(21) PRIMARY KEY NOT NULL,
    "appliedAt"     timestamptz NOT NULL DEFAULT now()
);
