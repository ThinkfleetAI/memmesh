// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Domain model for memory items. Column-for-column compatible with the
//! existing SaaS Postgres schema (`clawdbot_memory_item`,
//! `agent_memory_feedback`, `memory_audit_event`, etc.) so the sync layer
//! reconciles rows without translation.
//!
//! All field names map to the camelCase column names TypeORM produces
//! (the SaaS schema is camelCase, not snake_case).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Hierarchical scope. Lower entries are more specific and cascade up.
/// Persisted as a lowercase string column matching the TS `MemoryScope` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    Platform,
    Project,
    Location,
    Agent,
    User,
    Session,
}

impl MemoryScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Project => "project",
            Self::Location => "location",
            Self::Agent => "agent",
            Self::User => "user",
            Self::Session => "session",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryStatus {
    Pending,
    Confirmed,
    Superseded,
    Rejected,
}

impl MemoryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Superseded => "superseded",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryImpact {
    High,
    Low,
}

impl MemoryImpact {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Low => "low",
        }
    }
}

/// Feedback rating sentiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedbackKind {
    Positive,
    Negative,
}

/// Canonical memory record. Mirrors the `clawdbot_memory_item` Postgres
/// table column-for-column. The TS schema is camelCase; the Rust struct uses
/// snake_case fields with serde renames to match wire/JSON shapes when
/// serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryItem {
    pub id: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,

    pub platform_id: String,
    pub project_id: Option<String>,
    pub location_id: Option<String>,
    pub chatbot_id: Option<String>,
    pub chat_identity_id: Option<String>,
    pub session_key: Option<String>,

    /// Kind / category of memory. e.g. "fact", "preference", "rule".
    /// Maps to the Postgres column `type` (renamed because `type` is reserved
    /// in Rust).
    #[serde(rename = "type")]
    pub type_: String,

    pub content: String,
    pub category: Option<String>,

    /// 0.0 - 10.0 importance score (TS default 5).
    pub importance: f32,

    pub source: Option<String>,
    pub metadata: serde_json::Value,

    pub scope: MemoryScope,
    pub status: MemoryStatus,

    /// 0.0 - 1.0 confidence (TS default 1.0).
    pub confidence: f32,

    pub impact: Option<MemoryImpact>,

    pub superseded_by_id: Option<String>,
    pub confirmed_by_user_id: Option<String>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub negative_rating_count: i32,

    /// Bi-temporal interval — when the fact is/was true in the world.
    pub valid_from: DateTime<Utc>,
    pub valid_to: Option<DateTime<Utc>>,

    /// System time the fact was recorded.
    pub learned_at: DateTime<Utc>,
    /// Bumped on every retrieval — drives recency scoring.
    pub last_accessed_at: DateTime<Utc>,
}

impl MemoryItem {
    /// Construct a new memory item with sensible defaults for required
    /// fields. The caller provides identity + content; everything else
    /// gets the same defaults the TS service uses.
    pub fn new(
        id: impl Into<String>,
        platform_id: impl Into<String>,
        type_: impl Into<String>,
        content: impl Into<String>,
        scope: MemoryScope,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: id.into(),
            created: now,
            updated: now,
            platform_id: platform_id.into(),
            project_id: None,
            location_id: None,
            chatbot_id: None,
            chat_identity_id: None,
            session_key: None,
            type_: type_.into(),
            content: content.into(),
            category: None,
            importance: 5.0,
            source: None,
            metadata: serde_json::Value::Null,
            scope,
            status: MemoryStatus::Confirmed,
            confidence: 1.0,
            impact: None,
            superseded_by_id: None,
            confirmed_by_user_id: None,
            confirmed_at: None,
            negative_rating_count: 0,
            valid_from: now,
            valid_to: None,
            learned_at: now,
            last_accessed_at: now,
        }
    }
}
