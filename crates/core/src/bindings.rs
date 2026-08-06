// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Per-machine mapping from a working-directory path to a SaaS
//! `(platformId, projectId)` pair.
//!
//! Bindings exist so that memories written from a given local repo always tag
//! with the same SaaS-resolved project across sessions and tools. They are
//! intentionally local-only — `cwd` paths don't make sense on other machines
//! and are not synced.
//!
//! The resolver in `crates/storage` does the lookup (exact match on `cwd`,
//! optionally walking up the directory tree to inherit a parent binding).
//! This module just defines the type that flows between storage, MCP, CLI,
//! and the eventual desktop UI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBinding {
    /// Absolute filesystem path. The resolver matches this against the
    /// caller's current working directory.
    pub cwd: String,
    /// SaaS platform id. Stored on the binding (rather than read from
    /// global config at lookup time) so a single machine could in theory
    /// hold bindings for multiple platforms — useful for users belonging
    /// to more than one organization.
    pub platform_id: String,
    /// SaaS project id.
    pub project_id: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

/// Well-known keys in the `sync_state` table. The table itself is free-form
/// key/value; this enum is the documented set of keys the engine reads/writes.
/// Stored as strings (not enum discriminants) so a future reader on an older
/// build never panics on an unknown key.
pub mod sync_state_key {
    /// Opaque cursor returned by the SaaS pull endpoint. Pass back on the
    /// next pull. Empty string means "start from the beginning."
    pub const PULL_CURSOR: &str = "pull_cursor";

    /// ISO-8601 UTC timestamp of the most recent successful push response.
    pub const LAST_PUSH_AT: &str = "last_push_at";

    /// ISO-8601 UTC timestamp of the most recent successful pull response.
    pub const LAST_PULL_AT: &str = "last_pull_at";

    /// Cached plan tier from the most recent token validation: `"free"` or
    /// `"paid"`. The SaaS is the source of truth — this is a hot-path cache
    /// so the cap check doesn't have to round-trip on every save.
    pub const PLAN_TIER: &str = "plan_tier";

    /// Cached signed-in platform id (mirror of `config.toml`'s `[sync]
    /// platform_id` so cap and binding paths don't have to re-parse TOML).
    pub const PLATFORM_ID: &str = "platform_id";
}

/// Per-row sync state on `clawdbot_memory_item`. Stored as a string column
/// (`sync_status`) rather than an enum discriminant for the same
/// forward-compat reason as `sync_state_key`.
pub mod sync_status {
    /// Row needs to be pushed (new write or modification since the last
    /// successful push).
    pub const PENDING: &str = "pending";

    /// Row has been pushed and the server acknowledged it.
    pub const SYNCED: &str = "synced";

    /// Server rejected the push (e.g. project binding invalid, permission
    /// denied). Needs manual resolution before sync will retry.
    pub const CONFLICT: &str = "conflict";
}
