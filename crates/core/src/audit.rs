// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Memory access audit log — append-only record of every read / write /
//! subscribe routed through the engine. Mirrors `memory_audit_event` in the
//! SaaS Postgres schema.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryAuditEvent {
    pub id: String,
    pub created: DateTime<Utc>,
    pub platform_id: String,
    pub project_id: Option<String>,
    /// Free-form actor identifier. Recommended conventions:
    ///   `user:<id>`, `agent:<id>`, `mcp:<tool>`, `system:<process>`.
    pub actor: String,
    /// Event type tag. Recommended values: `read`, `write`, `search`,
    /// `subscribe`, `supersede`, `delete`, `migrate`.
    pub event_type: String,
    pub query: Option<String>,
    pub scope: Option<String>,
    /// Comma-separated list of memory item IDs touched (TS uses TEXT, not
    /// an array, for cross-backend simplicity).
    pub memory_ids: Option<String>,
    pub result_count: i32,
    pub metadata: serde_json::Value,
}
