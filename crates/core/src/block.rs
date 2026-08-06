// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Memory block — labeled scratchpad. Mirrors `agent_memory_block`.
//! Useful for "always-visible" facts the agent should keep top-of-mind
//! (e.g. user's current OKR, an active incident's status).

use crate::MemoryScope;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBlock {
    pub id: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub platform_id: String,
    pub project_id: Option<String>,
    pub location_id: Option<String>,
    pub chatbot_id: Option<String>,
    pub chat_identity_id: Option<String>,
    pub session_key: Option<String>,
    pub scope: MemoryScope,
    pub label: String,
    pub value: String,
    pub size_limit_tokens: i32,
    pub importance: f32,
    pub last_edited_by: Option<String>,
    pub last_edited_at: Option<DateTime<Utc>>,
    pub version: i32,
}
