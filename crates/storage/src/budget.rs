// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Per-platform monthly cost budget.
//!
//! Single source of truth: any code path about to spend on a paid API
//! (Anthropic, OpenAI, Voyage, ZeroEntropy, etc.) calls `gate()` with
//! an estimated cost in USD cents. If the platform's cumulative spend
//! for the current month would exceed its cap, the call is refused
//! with `BudgetExceeded`. After the call succeeds, the caller invokes
//! `record_cost()` with the actual cost.
//!
//! Today the engine has no LLM call sites of its own (extraction is
//! pure regex). This module is the gate that will catch every
//! synthesize / embed / graph-extract-with-LLM operation as they
//! land, so we never repeat the "MCP-connected agent silently burns
//! Anthropic credits" pattern that gbrain's protected-cycle-phases
//! comment calls out.
//!
//! State lives in the existing `sync_state` k/v table — no new tables.
//! Keys are namespaced by platform so multi-tenant deployments
//! (Postgres backend / SaaS server) get independent counters.

use crate::{Storage, StorageError};
use chrono::{DateTime, Datelike, Utc};

/// One-time cost report. Returned by `gate()` so callers can log
/// what they're about to spend, then again by `record_cost()` with
/// the actual cost (in case it differed from the estimate).
#[derive(Debug, Clone, Copy)]
pub struct BudgetState {
    pub period_start: DateTime<Utc>,
    pub spent_cents: i64,
    pub cap_cents: Option<i64>,
}

/// Refusal returned by `gate()` when an operation would push spend
/// past the cap. The caller decides what to do — typically log a
/// loud warning and skip the operation.
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    #[error(
        "budget exceeded for platform {platform_id}: \
         spent ${spent:.2}, attempting +${attempting:.2}, cap ${cap:.2} \
         (period starts {period_start}). \
         Raise the cap with `memmesh budget set-cap --usd <N>` \
         or wait until the next month."
    )]
    Exceeded {
        platform_id: String,
        spent: f64,
        attempting: f64,
        cap: f64,
        period_start: String,
    },
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Check whether `platform_id` can spend `cents` more this month
/// without breaching its cap. Returns `Ok(state)` if allowed (call
/// `record_cost` after the actual spend completes); returns
/// `Err(Exceeded)` if it would push past the cap.
///
/// A platform with no cap set is allowed any spend — the SaaS sets
/// caps at sign-up time; engines running locally without a cap are
/// trusted by default.
///
/// Period boundary handling: if the stored `period_start` is from a
/// previous calendar month, the counter resets to 0 before the check.
/// First call ever for a platform initializes state.
pub async fn gate<S: Storage>(
    storage: &S,
    platform_id: &str,
    cents: i64,
) -> Result<BudgetState, BudgetError> {
    let mut state = load_state(storage, platform_id).await?;
    state = maybe_roll_period(state);

    if let Some(cap) = state.cap_cents {
        let projected = state.spent_cents.saturating_add(cents);
        if projected > cap {
            return Err(BudgetError::Exceeded {
                platform_id: platform_id.to_string(),
                spent: cents_to_usd(state.spent_cents),
                attempting: cents_to_usd(cents),
                cap: cents_to_usd(cap),
                period_start: state.period_start.to_rfc3339(),
            });
        }
    }
    Ok(state)
}

/// Add `cents` to the platform's spend. Idempotent only if the caller
/// makes it so — record once per successful API call.
pub async fn record_cost<S: Storage>(
    storage: &S,
    platform_id: &str,
    cents: i64,
) -> Result<BudgetState, BudgetError> {
    let mut state = load_state(storage, platform_id).await?;
    state = maybe_roll_period(state);
    state.spent_cents = state.spent_cents.saturating_add(cents);
    save_state(storage, platform_id, &state).await?;
    Ok(state)
}

/// Set (or clear) the monthly cap for `platform_id`. `None` removes
/// the cap and reverts to unlimited spend.
pub async fn set_cap<S: Storage>(
    storage: &S,
    platform_id: &str,
    cap_cents: Option<i64>,
) -> Result<(), BudgetError> {
    let mut state = load_state(storage, platform_id).await?;
    state.cap_cents = cap_cents;
    save_state(storage, platform_id, &state).await?;
    Ok(())
}

/// Read current state for `platform_id` without mutating anything.
/// Auto-rolls the period if it crossed a month boundary since the
/// last write (the rolled state isn't persisted until the next
/// `record_cost` / `set_cap` call — fine because spend reads as 0
/// in the new period either way).
pub async fn current<S: Storage>(
    storage: &S,
    platform_id: &str,
) -> Result<BudgetState, BudgetError> {
    let state = load_state(storage, platform_id).await?;
    Ok(maybe_roll_period(state))
}

// ─── Internals ──────────────────────────────────────────────────────

fn cents_to_usd(c: i64) -> f64 {
    c as f64 / 100.0
}

fn period_start_for(now: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::NaiveDate;
    let start = NaiveDate::from_ymd_opt(now.year(), now.month(), 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    DateTime::<Utc>::from_naive_utc_and_offset(start, Utc)
}

fn maybe_roll_period(mut state: BudgetState) -> BudgetState {
    let current_period = period_start_for(Utc::now());
    if state.period_start < current_period {
        state.period_start = current_period;
        state.spent_cents = 0;
        // cap_cents is preserved across rollover — it's a policy
        // decision, not a counter.
    }
    state
}

fn key_period(platform_id: &str) -> String {
    format!("budget_period_start:{platform_id}")
}
fn key_spent(platform_id: &str) -> String {
    format!("budget_spent_cents:{platform_id}")
}
fn key_cap(platform_id: &str) -> String {
    format!("budget_cap_cents:{platform_id}")
}

async fn load_state<S: Storage>(
    storage: &S,
    platform_id: &str,
) -> Result<BudgetState, StorageError> {
    let period_raw = storage.get_sync_state(&key_period(platform_id)).await?;
    let spent_raw = storage.get_sync_state(&key_spent(platform_id)).await?;
    let cap_raw = storage.get_sync_state(&key_cap(platform_id)).await?;

    let period_start = period_raw
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|| period_start_for(Utc::now()));
    let spent_cents = spent_raw
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let cap_cents = cap_raw.as_deref().and_then(|s| s.parse::<i64>().ok());

    Ok(BudgetState {
        period_start,
        spent_cents,
        cap_cents,
    })
}

async fn save_state<S: Storage>(
    storage: &S,
    platform_id: &str,
    state: &BudgetState,
) -> Result<(), StorageError> {
    storage
        .set_sync_state(&key_period(platform_id), &state.period_start.to_rfc3339())
        .await?;
    storage
        .set_sync_state(&key_spent(platform_id), &state.spent_cents.to_string())
        .await?;
    if let Some(cap) = state.cap_cents {
        storage
            .set_sync_state(&key_cap(platform_id), &cap.to_string())
            .await?;
    }
    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStore;

    async fn make_store() -> SqliteStore {
        let s = SqliteStore::connect("sqlite::memory:").await.unwrap();
        s.migrate().await.unwrap();
        s
    }

    #[tokio::test]
    async fn no_cap_allows_any_spend() {
        let s = make_store().await;
        let state = gate(&s, "plat_a", 1_000_000).await.unwrap();
        assert_eq!(state.spent_cents, 0);
        assert_eq!(state.cap_cents, None);
    }

    #[tokio::test]
    async fn cap_blocks_overspend() {
        let s = make_store().await;
        set_cap(&s, "plat_a", Some(500)).await.unwrap(); // $5.00 cap
        record_cost(&s, "plat_a", 300).await.unwrap(); // $3.00 spent
        let err = gate(&s, "plat_a", 300).await.unwrap_err();
        assert!(matches!(err, BudgetError::Exceeded { .. }));
    }

    #[tokio::test]
    async fn record_cost_accumulates() {
        let s = make_store().await;
        set_cap(&s, "plat_b", Some(1000)).await.unwrap();
        record_cost(&s, "plat_b", 100).await.unwrap();
        record_cost(&s, "plat_b", 200).await.unwrap();
        let state = current(&s, "plat_b").await.unwrap();
        assert_eq!(state.spent_cents, 300);
        assert_eq!(state.cap_cents, Some(1000));
    }

    #[tokio::test]
    async fn under_cap_allows_spend() {
        let s = make_store().await;
        set_cap(&s, "plat_c", Some(1000)).await.unwrap();
        record_cost(&s, "plat_c", 500).await.unwrap();
        let state = gate(&s, "plat_c", 200).await.unwrap();
        assert_eq!(state.spent_cents, 500); // gate doesn't mutate
        assert_eq!(state.cap_cents, Some(1000));
    }

    #[tokio::test]
    async fn separate_platforms_independent() {
        let s = make_store().await;
        set_cap(&s, "plat_x", Some(100)).await.unwrap();
        set_cap(&s, "plat_y", Some(100)).await.unwrap();
        record_cost(&s, "plat_x", 50).await.unwrap();
        let x = current(&s, "plat_x").await.unwrap();
        let y = current(&s, "plat_y").await.unwrap();
        assert_eq!(x.spent_cents, 50);
        assert_eq!(y.spent_cents, 0);
    }
}
