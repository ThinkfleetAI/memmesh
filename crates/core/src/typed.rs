// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Typed-attribute domain model — the keystone that lets the engine reason
//! over structured/numeric data (credit scores, sensor readings, balances)
//! instead of opaque `metadata` JSON.
//!
//! Three records mirror the Postgres tables in `0004_typed_attributes.sql`:
//!   - [`AttributeDef`]    → `memory_attribute_registry`
//!   - [`TypedObservation`]→ `memory_typed_observation`
//!   - [`Accumulator`]     → `memory_attribute_accumulator`
//!
//! Pure domain code only: the `DataType` match rules and the [`Accumulator`]
//! derived statistics (mean/variance/stddev) live here so they're trivially
//! testable; all I/O lives in `memory-storage`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The declared type of an attribute. Persisted as a lowercase string matching
/// the `dataType` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataType {
    Numeric,
    Categorical,
    Temporal,
    Boolean,
}

impl DataType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Numeric => "numeric",
            Self::Categorical => "categorical",
            Self::Temporal => "temporal",
            Self::Boolean => "boolean",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "numeric" => Some(Self::Numeric),
            "categorical" => Some(Self::Categorical),
            "temporal" => Some(Self::Temporal),
            "boolean" => Some(Self::Boolean),
            _ => None,
        }
    }
}

/// Ingest acceptance status for a typed observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObservationStatus {
    Accepted,
    Quarantined,
}

impl ObservationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Quarantined => "quarantined",
        }
    }
}

/// A per-`(platform, project)` attribute definition. Drives input validation:
/// type match, plausibility range, required-ness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeDef {
    pub id: String,
    pub platform_id: String,
    pub project_id: Option<String>,
    pub attribute_key: String,
    pub data_type: DataType,
    pub unit: Option<String>,
    /// Inclusive plausibility bounds for numeric/temporal attributes. A value
    /// outside `[min_valid, max_valid]` is quarantined, not stored as truth.
    pub min_valid: Option<f64>,
    pub max_valid: Option<f64>,
    pub required: bool,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl AttributeDef {
    /// Validate a value against this definition. Returns `Ok(())` if the value
    /// is well-typed and within bounds, or `Err(reason)` describing why it
    /// should be quarantined. Pure — no I/O.
    pub fn validate(&self, obs: &TypedObservation) -> Result<(), String> {
        match self.data_type {
            DataType::Numeric => {
                let v = obs
                    .value_numeric
                    .ok_or_else(|| "expected numeric value, got none".to_string())?;
                if !v.is_finite() {
                    return Err(format!("non-finite numeric value: {v}"));
                }
                if let Some(min) = self.min_valid {
                    if v < min {
                        return Err(format!("value {v} below minValid {min}"));
                    }
                }
                if let Some(max) = self.max_valid {
                    if v > max {
                        return Err(format!("value {v} above maxValid {max}"));
                    }
                }
            }
            DataType::Boolean => {
                if obs.value_bool.is_none() {
                    return Err("expected boolean value, got none".to_string());
                }
            }
            DataType::Categorical => {
                if obs.value_text.as_deref().unwrap_or("").is_empty() {
                    return Err("expected non-empty categorical value, got none".to_string());
                }
            }
            DataType::Temporal => {
                if obs.value_ts.is_none() {
                    return Err("expected temporal value, got none".to_string());
                }
            }
        }
        Ok(())
    }
}

/// A single typed measurement of an attribute for a subject at a point in time.
/// Exactly one `value_*` field is meaningful, selected by the attribute's
/// declared [`DataType`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypedObservation {
    pub id: String,
    pub platform_id: String,
    pub project_id: Option<String>,
    pub subject_kind: String,
    pub subject_external_id: String,
    pub attribute_key: String,

    pub value_numeric: Option<f64>,
    pub value_text: Option<String>,
    pub value_bool: Option<bool>,
    pub value_ts: Option<DateTime<Utc>>,

    pub observed_at: DateTime<Utc>,
    pub source: Option<String>,

    /// Source-trust weight in `0..=1` (default 1.0). Carried into derivations.
    #[serde(default = "one_f32")]
    pub trust: f32,
    /// Computed completeness × validity score in `0..=1` (set by the validator).
    #[serde(default = "one_f32")]
    pub quality_score: f32,
    /// Acceptance status, set by the validator. Quarantined rows are stored for
    /// audit but excluded from accumulation/derivation.
    #[serde(default = "accepted")]
    pub status: ObservationStatus,

    #[serde(default = "Utc::now")]
    pub ingested_at: DateTime<Utc>,
}

fn one_f32() -> f32 {
    1.0
}
fn accepted() -> ObservationStatus {
    ObservationStatus::Accepted
}

impl TypedObservation {
    /// Minimal numeric observation with defaults; callers set identity + value.
    pub fn numeric(
        id: impl Into<String>,
        platform_id: impl Into<String>,
        subject_kind: impl Into<String>,
        subject_external_id: impl Into<String>,
        attribute_key: impl Into<String>,
        value: f64,
        observed_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            platform_id: platform_id.into(),
            project_id: None,
            subject_kind: subject_kind.into(),
            subject_external_id: subject_external_id.into(),
            attribute_key: attribute_key.into(),
            value_numeric: Some(value),
            value_text: None,
            value_bool: None,
            value_ts: None,
            observed_at,
            source: None,
            trust: 1.0,
            quality_score: 1.0,
            status: ObservationStatus::Accepted,
            ingested_at: observed_at,
        }
    }
}

/// Per-`(subject, attribute)` running statistics, updated O(1) per observation.
/// Mirrors `memory_attribute_accumulator`. The raw moments (`count`, `sum`,
/// `sum_sq`) are stored; mean/variance/stddev are derived on read.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accumulator {
    pub platform_id: String,
    pub subject_kind: String,
    pub subject_external_id: String,
    pub attribute_key: String,
    pub project_id: Option<String>,

    pub count: i64,
    pub sum: f64,
    pub sum_sq: f64,
    pub min_val: Option<f64>,
    pub max_val: Option<f64>,
    pub last_val: Option<f64>,
    pub last_observed_at: Option<DateTime<Utc>>,
    /// Running total — the substrate for cumulative-to-limit primitives.
    pub cumulative: f64,
    pub ewma: Option<f64>,
    pub ewma_var: Option<f64>,
    pub updated: DateTime<Utc>,
}

/// EWMA smoothing factor. Kept in sync with the SQL accumulate() upsert in
/// `memory-storage::postgres` — this Rust fold is the canonical reference the
/// SQL mirrors, and the accuracy suite asserts they agree.
pub const EWMA_ALPHA: f64 = 0.3;

impl Accumulator {
    /// An empty accumulator for a subject+attribute, before any observation.
    pub fn empty(
        platform_id: impl Into<String>,
        subject_kind: impl Into<String>,
        subject_external_id: impl Into<String>,
        attribute_key: impl Into<String>,
    ) -> Self {
        Self {
            platform_id: platform_id.into(),
            subject_kind: subject_kind.into(),
            subject_external_id: subject_external_id.into(),
            attribute_key: attribute_key.into(),
            project_id: None,
            count: 0,
            sum: 0.0,
            sum_sq: 0.0,
            min_val: None,
            max_val: None,
            last_val: None,
            last_observed_at: None,
            cumulative: 0.0,
            ewma: None,
            ewma_var: None,
            updated: chrono::DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now),
        }
    }

    /// Fold one numeric observation into the running stats. This is the
    /// reference implementation of the incremental accumulation; the Postgres
    /// `accumulate()` upsert performs the identical arithmetic in SQL so that
    /// engine output is reproducible regardless of which path applied a value.
    pub fn fold(&mut self, value: f64, observed_at: DateTime<Utc>) {
        let prev_ewma = self.ewma;
        self.count += 1;
        self.sum += value;
        self.sum_sq += value * value;
        self.min_val = Some(self.min_val.map_or(value, |m| m.min(value)));
        self.max_val = Some(self.max_val.map_or(value, |m| m.max(value)));
        self.cumulative += value;
        // Last value wins by observed time (ties keep the newer fold).
        if self.last_observed_at.is_none_or(|t| observed_at >= t) {
            self.last_val = Some(value);
            self.last_observed_at = Some(observed_at);
        }
        // EWMA + EWMA-variance recursion (alpha = EWMA_ALPHA).
        let base = prev_ewma.unwrap_or(value);
        self.ewma = Some(EWMA_ALPHA * value + (1.0 - EWMA_ALPHA) * base);
        let diff = value - base;
        self.ewma_var =
            Some((1.0 - EWMA_ALPHA) * (self.ewma_var.unwrap_or(0.0) + EWMA_ALPHA * diff * diff));
        self.updated = observed_at;
    }

    /// Arithmetic mean, or `None` if no samples.
    pub fn mean(&self) -> Option<f64> {
        if self.count <= 0 {
            None
        } else {
            Some(self.sum / self.count as f64)
        }
    }

    /// Population variance from the stored moments, or `None` if no samples.
    /// `Var = E[x^2] - E[x]^2`, clamped at 0 to absorb float error.
    pub fn variance(&self) -> Option<f64> {
        let n = self.count as f64;
        if self.count <= 0 {
            return None;
        }
        let mean = self.sum / n;
        Some((self.sum_sq / n - mean * mean).max(0.0))
    }

    /// Population standard deviation.
    pub fn stddev(&self) -> Option<f64> {
        self.variance().map(f64::sqrt)
    }
}

/// Outcome of a batch ingest: how many rows were accepted, quarantined, or
/// skipped as duplicates (already in the ingest ledger).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestReport {
    pub accepted: u64,
    pub quarantined: u64,
    pub duplicates: u64,
    /// Per-row quarantine reasons, keyed by observation id.
    #[serde(default)]
    pub quarantine_reasons: std::collections::BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc(count: i64, sum: f64, sum_sq: f64) -> Accumulator {
        Accumulator {
            platform_id: "p".into(),
            subject_kind: "contact".into(),
            subject_external_id: "c1".into(),
            attribute_key: "credit_score".into(),
            project_id: None,
            count,
            sum,
            sum_sq,
            min_val: None,
            max_val: None,
            last_val: None,
            last_observed_at: None,
            cumulative: sum,
            ewma: None,
            ewma_var: None,
            updated: Utc::now(),
        }
    }

    #[test]
    fn mean_and_variance_from_moments() {
        // samples: 600, 650, 700 → mean 650, population variance 1666.67
        let n = 3.0;
        let sum = 600.0 + 650.0 + 700.0;
        let sum_sq = 600.0_f64.powi(2) + 650.0_f64.powi(2) + 700.0_f64.powi(2);
        let a = acc(3, sum, sum_sq);
        assert!((a.mean().unwrap() - 650.0).abs() < 1e-9);
        let expected_var =
            (600.0_f64 - 650.0).powi(2) + (650.0_f64 - 650.0).powi(2) + (700.0_f64 - 650.0).powi(2);
        assert!((a.variance().unwrap() - expected_var / n).abs() < 1e-6);
        assert!((a.stddev().unwrap() - (expected_var / n).sqrt()).abs() < 1e-6);
    }

    #[test]
    fn empty_accumulator_has_no_stats() {
        let a = acc(0, 0.0, 0.0);
        assert!(a.mean().is_none());
        assert!(a.variance().is_none());
    }

    #[test]
    fn validate_rejects_wrong_type() {
        let def = AttributeDef {
            id: "d".into(),
            platform_id: "p".into(),
            project_id: None,
            attribute_key: "active".into(),
            data_type: DataType::Boolean,
            unit: None,
            min_valid: None,
            max_valid: None,
            required: false,
            metadata: serde_json::Value::Null,
        };
        // numeric value supplied where a boolean is expected → rejected
        let obs = TypedObservation::numeric("o", "p", "contact", "c1", "active", 1.0, Utc::now());
        assert!(def.validate(&obs).is_err());
    }
}
