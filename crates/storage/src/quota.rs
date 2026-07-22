// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Entry cap enforcement.
//!
//! The open-source engine is **uncapped by default**: `free_tier.entry_cap`
//! defaults to `0` (unlimited), so `effective_cap` returns `None` and every
//! write is accepted. A cap only exists when a commercial embedder opts in —
//! by setting a positive `free_tier.entry_cap` (or `THINKFLEET_FREE_TIER_CAP`)
//! for desktop plan tiers, or in SaaS-connected mode where the plan tier
//! governs quota server-side instead.
//!
//! Every write path (observe, explicit save in CLI / MCP / HTTP) calls
//! `ensure_under_cap` first. When a cap is set, reaching it is a soft failure:
//! the write is rejected with a clear message; existing memories are untouched.

use crate::{Storage, StorageError};

/// Quota check failure: distinct from generic `StorageError` so call sites
/// can surface a user-actionable message ("upgrade or delete some
/// memories") instead of a generic DB error.
#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error(
        "memory cap reached: {count} memories at cap {cap}. \
         Raise `free_tier.entry_cap` (set it to 0 for unlimited), or \
         delete some memories to make room (`memmesh delete <id>`)."
    )]
    CapReached { count: i64, cap: i64 },

    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Check whether the engine is under its quota for new writes.
///
/// `cap = None` → no enforcement (paid tier). Otherwise compares the live
/// item count against `cap`. Returns `Ok(())` if the next write is
/// permitted; `Err(CapReached)` if the cap is already reached.
pub async fn ensure_under_cap(
    store: &dyn Storage,
    cap: Option<i64>,
) -> Result<(), QuotaError> {
    let Some(cap) = cap else {
        return Ok(());
    };
    let count = store.count_items().await?;
    if count >= cap {
        return Err(QuotaError::CapReached { count, cap });
    }
    Ok(())
}

/// Resolve the effective cap from a config. `None` = unlimited.
///
/// SaaS-connected mode → `None` (the server-side plan tier governs quota).
/// Local mode → `None` unless `free_tier.entry_cap` is a positive value, in
/// which case that cap applies. The default `entry_cap` is `0` (unlimited),
/// so the open-source engine is uncapped; a positive value is an explicit
/// opt-in for commercial embedders.
pub fn effective_cap(config: &memory_core::config::Config) -> Option<i64> {
    if config.is_saas_configured() {
        return None;
    }
    match config.free_tier.entry_cap {
        cap if cap > 0 => Some(cap),
        _ => None,
    }
}
