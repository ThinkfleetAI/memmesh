// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Per-identity behavior profile. Mirrors `agent_user_profile` in the SaaS
//! schema. Unique on (chatIdentityId, projectId).
//!
//! The `profile` blob is the lattice engine's output for one user — accumulated
//! preferences, recurring behaviors, segments, etc. Format is intentionally
//! free-form JSON because the lattice writers + downstream consumers evolve
//! together.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub id: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub chat_identity_id: String,
    pub project_id: String,
    pub platform_id: String,
    pub profile: serde_json::Value,
    pub profile_version: i32,
    pub last_updated_from_session_id: Option<String>,
}
