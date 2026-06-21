// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Free-tier entry cap enforcement.
//!
//! In local-only mode the engine accepts up to `Config::free_tier.entry_cap`
//! memory items (default 500, confirmed 2026-05-18). In SaaS-connected mode
//! the cap is lifted — the SaaS-side plan tier governs quota instead.
//!
//! Every write path (observe, explicit save in CLI / MCP / HTTP) calls
//! `ensure_under_cap` first. Reaching the cap is a soft failure: the write
//! is rejected with a clear message; existing memories are untouched.

use crate::{Storage, StorageError};

/// Quota check failure: distinct from generic `StorageError` so call sites
/// can surface a user-actionable message ("upgrade or delete some
/// memories") instead of a generic DB error.
#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error(
        "memory cap reached: {count} memories at cap {cap}. \
         Upgrade your plan at https://memmesh.ai to lift the \
         cap, or delete some memories to make room \
         (`memmesh delete <id>`)."
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

/// Resolve the effective cap from a config. SaaS-connected mode → `None`
/// (no cap). Local-only mode → `Some(free_tier.entry_cap)`.
///
/// This is the helper the write paths use so they don't have to repeat the
/// "is the engine in SaaS mode?" check.
pub fn effective_cap(config: &memory_core::config::Config) -> Option<i64> {
    if config.is_saas_configured() {
        None
    } else {
        Some(config.free_tier.entry_cap)
    }
}
