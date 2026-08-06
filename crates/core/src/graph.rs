// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Knowledge-graph entities + edges. Mirrors `memory_entity` and
//! `memory_edge`. Both tables are bi-temporal — `valid_from` / `valid_to`
//! mark when the fact was true in the world.

use crate::MemoryScope;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntity {
    pub id: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub platform_id: String,
    pub project_id: Option<String>,
    pub location_id: Option<String>,
    pub chatbot_id: Option<String>,
    pub chat_identity_id: Option<String>,
    pub scope: MemoryScope,
    #[serde(rename = "type")]
    pub type_: String,
    pub canonical_name: String,
    /// JSON-encoded array (in Postgres this is text[]; in SQLite it's a TEXT
    /// JSON array — the engine reads/writes it as Vec<String>).
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub metadata: serde_json::Value,
    pub valid_from: DateTime<Utc>,
    pub valid_to: Option<DateTime<Utc>>,
    pub superseded_by_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEdge {
    pub id: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub platform_id: String,
    pub project_id: Option<String>,
    pub location_id: Option<String>,
    pub chatbot_id: Option<String>,
    pub chat_identity_id: Option<String>,
    pub scope: MemoryScope,
    pub subject_id: String,
    pub predicate: String,
    /// Either `object_id` (FK to entity) or `object_literal` (free-form
    /// string) is set, never both — the DB enforces this via a CHECK
    /// constraint. Construct with the helper methods.
    pub object_id: Option<String>,
    pub object_literal: Option<String>,
    pub weight: f32,
    pub source_memory_id: Option<String>,
    pub metadata: serde_json::Value,
    pub valid_from: DateTime<Utc>,
    pub valid_to: Option<DateTime<Utc>>,
}
