// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Contact + contact event domain types. Mirrors `clawdbot_contact` and
//! `clawdbot_contact_event` in the SaaS schema. These are read-heavy from
//! the lattice engine's perspective (pattern detection scans contact
//! events).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub id: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub chatbot_id: String,
    pub project_id: Option<String>,
    pub name: String,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub relationship: Option<String>,
    pub is_emergency: bool,
    pub priority: i32,
    pub notes: Option<String>,
    pub metadata: serde_json::Value,
    pub external_ids: serde_json::Value,
    /// JSON array of free-form tags.
    pub tags: serde_json::Value,
    pub segment: Option<String>,
    pub lifetime_value: Option<f64>,
    pub last_interaction_at: Option<DateTime<Utc>>,
    pub chat_identity_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactEvent {
    pub id: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub contact_id: String,
    pub project_id: String,
    pub chatbot_id: Option<String>,
    pub event_type: String,
    pub title: String,
    pub description: Option<String>,
    pub event_data: serde_json::Value,
    pub occurred_at: DateTime<Utc>,
    pub source: Option<String>,
    pub engagement_rule_id: Option<String>,
    pub engagement_execution_id: Option<String>,
}
