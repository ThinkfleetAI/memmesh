// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Storage-layer query types. These are deliberately schema-shaped, not
//! domain-shaped — they describe what rows to fetch, not what scoring to
//! apply. Scoring + ranking belongs in `memory-core::scoring`.

use memory_core::{FeedbackKind, MemoryItem, MemoryScope, MemoryStatus};
use serde::{Deserialize, Serialize};

/// A memory item paired with its similarity to a query vector. `similarity` is
/// cosine in `-1..=1` (higher = closer). Returned by `Storage::vector_search`,
/// closest first.
#[derive(Debug, Clone)]
pub struct ScoredMemoryItem {
    pub item: MemoryItem,
    pub similarity: f32,
}

/// Aggregate counts for an admin observability dashboard, scoped to a
/// `MemoryFilter` (typically platform_id + optional project_id). Maps use
/// `BTreeMap` so the output is deterministically ordered.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryStats {
    pub total: i64,
    pub by_scope: std::collections::BTreeMap<String, i64>,
    pub by_status: std::collections::BTreeMap<String, i64>,
    pub by_type: std::collections::BTreeMap<String, i64>,
    /// Items whose `embedding` column is populated (semantically indexed).
    pub with_embedding: i64,
    /// Items still missing an embedding (the backfill work-list size).
    pub without_embedding: i64,
    /// Persisted `behavior_pattern` items (the lattice output).
    pub pattern_count: i64,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryQuery {
    pub filter: MemoryFilter,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Filter for `Storage::query`. `Deserialize` so the HTTP API can accept it
/// directly as a JSON body via `axum::Json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFilter {
    pub scope: Option<MemoryScope>,
    pub platform_id: Option<String>,
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub session_id: Option<String>,
    pub status: Option<MemoryStatus>,
    pub kind: Option<String>,
    pub text_match: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub platform_id: Option<String>,
    pub project_id: Option<String>,
    pub actor: Option<String>,
    pub event_type: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct FeedbackFilter {
    pub memory_id: Option<String>,
    pub created_by_user_id: Option<String>,
    pub rating: Option<FeedbackKind>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct ContactFilter {
    pub chatbot_id: Option<String>,
    pub project_id: Option<String>,
    pub chat_identity_id: Option<String>,
    pub email: Option<String>,
    pub segment: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct ContactEventFilter {
    pub contact_id: Option<String>,
    pub project_id: Option<String>,
    pub event_type: Option<String>,
    /// Inclusive lower bound on occurredAt.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Exclusive upper bound on occurredAt.
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct BlockFilter {
    pub platform_id: Option<String>,
    pub project_id: Option<String>,
    pub chatbot_id: Option<String>,
    pub chat_identity_id: Option<String>,
    pub scope: Option<MemoryScope>,
    pub label: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct EntityFilter {
    pub platform_id: Option<String>,
    pub project_id: Option<String>,
    pub scope: Option<MemoryScope>,
    pub type_: Option<String>,
    pub canonical_name: Option<String>,
    /// Alias lookup — matches rows where `canonical_name = alias` OR
    /// `alias` appears anywhere in the `aliases` JSON array. Used by
    /// the entity resolver to find existing entities by ANY name they
    /// go by (nickname, email, social handle, misspelling, etc.).
    pub alias: Option<String>,
    /// When true, exclude rows where `valid_to` is set (i.e. only current
    /// facts). Default false returns all.
    pub current_only: bool,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct EdgeFilter {
    pub platform_id: Option<String>,
    pub project_id: Option<String>,
    pub scope: Option<MemoryScope>,
    pub subject_id: Option<String>,
    pub object_id: Option<String>,
    pub predicate: Option<String>,
    /// Only edges still open (`validTo IS NULL`). Superseded by `as_of`
    /// when that is set (a point in time is more specific than "now").
    pub current_only: bool,
    /// Point-in-time query: return edges that were valid AT this instant —
    /// `validFrom <= as_of AND (validTo IS NULL OR validTo > as_of)`. This is
    /// what makes the graph a *temporal* KG: "what did we believe on date X".
    pub as_of: Option<chrono::DateTime<chrono::Utc>>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Filter for `Storage::query_typed_observations`. `platform_id` is required in
/// practice (it's the partition key); the rest narrow by subject, attribute,
/// observed-at window, and numeric-value range. All bounds are inclusive.
#[derive(Debug, Clone, Default)]
pub struct TypedObservationFilter {
    pub platform_id: Option<String>,
    pub project_id: Option<String>,
    pub subject_kind: Option<String>,
    pub subject_external_id: Option<String>,
    pub attribute_key: Option<String>,
    /// Inclusive lower bound on observedAt.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Inclusive upper bound on observedAt.
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    /// Inclusive lower bound on valueNumeric.
    pub min_value: Option<f64>,
    /// Inclusive upper bound on valueNumeric.
    pub max_value: Option<f64>,
    /// When set, restrict to this acceptance status (e.g. only `accepted`).
    pub status: Option<memory_core::ObservationStatus>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Filter for `Storage::query_attribute_defs` (the per-project registry).
#[derive(Debug, Clone, Default)]
pub struct AttributeDefFilter {
    pub platform_id: Option<String>,
    pub project_id: Option<String>,
    pub attribute_key: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Identity of a subject + attribute, used to read a single accumulator.
#[derive(Debug, Clone, Default)]
pub struct AccumulatorKey {
    pub platform_id: String,
    pub subject_kind: String,
    pub subject_external_id: String,
    pub attribute_key: String,
}
