// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Memory feedback ratings — mirrors `agent_memory_feedback` in the SaaS
//! schema. Negative ratings additionally bump the parent memory's
//! `negativeRatingCount` counter (a Storage method handles that atomically).

use crate::FeedbackKind;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFeedback {
    pub id: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub memory_id: String,
    pub response_id: Option<String>,
    pub rating: FeedbackKind,
    pub comment: Option<String>,
    pub created_by_user_id: String,
}
