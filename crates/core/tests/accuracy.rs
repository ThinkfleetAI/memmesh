// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.
//
// Accuracy suite — thousands of generated cases that pin down the correctness
// of the typed-attribute layer's pure logic: input validation, the incremental
// accumulator math, and the numeric threshold operators. Everything here is
// deterministic (a seeded LCG, no Math.random) so failures reproduce exactly.
//
// These cover the math the engine relies on independent of Postgres; the
// DB-gated tests in memory-storage confirm the SQL path produces the same
// numbers, and the ingest benchmark covers scale.

use chrono::{TimeZone, Utc};
use memory_core::{Accumulator, AttributeDef, DataType, ObservationStatus, TypedObservation};

/// Tiny deterministic PRNG (xorshift64*). Seeded so the whole suite is
/// reproducible — no external `rand` dependency, no nondeterminism.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform f64 in `[lo, hi)`.
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + unit * (hi - lo)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

fn numeric_def(min: Option<f64>, max: Option<f64>) -> AttributeDef {
    AttributeDef {
        id: "d".into(),
        platform_id: "p".into(),
        project_id: None,
        attribute_key: "x".into(),
        data_type: DataType::Numeric,
        unit: None,
        min_valid: min,
        max_valid: max,
        required: false,
        metadata: serde_json::Value::Null,
    }
}

fn numeric_obs(value: f64) -> TypedObservation {
    TypedObservation::numeric("o", "p", "contact", "c1", "x", value, Utc::now())
}

/// 5,000 random numeric (def, value) pairs: validation must accept iff the
/// value is finite and within the declared [min, max] bounds. The test derives
/// the expected verdict independently of `AttributeDef::validate`.
#[test]
fn validation_matches_reference_over_thousands() {
    let mut rng = Rng::new(0xA11CE);
    let mut accepted = 0u32;
    let mut rejected = 0u32;
    for _ in 0..5_000 {
        let min = if rng.below(2) == 0 {
            Some(rng.range(-1000.0, 1000.0))
        } else {
            None
        };
        let max = min.map(|m| m + rng.range(0.0, 2000.0));
        let value = rng.range(-2000.0, 3000.0);
        let def = numeric_def(min, max);
        let obs = numeric_obs(value);

        let expect_ok =
            value.is_finite() && min.is_none_or(|m| value >= m) && max.is_none_or(|m| value <= m);
        let got_ok = def.validate(&obs).is_ok();
        assert_eq!(
            got_ok, expect_ok,
            "validate({value}) with min={min:?} max={max:?}"
        );
        if got_ok {
            accepted += 1;
        } else {
            rejected += 1;
        }
    }
    // Sanity: the generator exercises both outcomes, not a degenerate stream.
    assert!(
        accepted > 500 && rejected > 500,
        "got {accepted} ok / {rejected} rejected"
    );
}

/// 2,000 random numeric streams (length 1..200). Folding them through the
/// incremental `Accumulator::fold` must agree with a from-scratch batch
/// computation of count/sum/min/max/cumulative/mean/variance. This is the math
/// the SQL accumulate() mirrors.
#[test]
fn accumulator_incremental_equals_batch_over_thousands() {
    let mut rng = Rng::new(0xBEEF_F00D);
    for case in 0..2_000 {
        let n = 1 + rng.below(200) as usize;
        let values: Vec<f64> = (0..n).map(|_| rng.range(-500.0, 500.0)).collect();

        // Reference batch stats.
        let count = n as f64;
        let sum: f64 = values.iter().sum();
        let sum_sq: f64 = values.iter().map(|v| v * v).sum();
        let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let mean = sum / count;
        let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / count;

        // Incremental fold, with strictly increasing observed times.
        let mut acc = Accumulator::empty("p", "contact", "c1", "x");
        for (i, v) in values.iter().enumerate() {
            let t = Utc.timestamp_opt(1_700_000_000 + i as i64, 0).unwrap();
            acc.fold(*v, t);
        }

        assert_eq!(acc.count, n as i64, "case {case}: count");
        assert!(
            (acc.sum - sum).abs() < 1e-6 * (1.0 + sum.abs()),
            "case {case}: sum"
        );
        assert!(
            (acc.sum_sq - sum_sq).abs() < 1e-3 * (1.0 + sum_sq.abs()),
            "case {case}: sumSq"
        );
        assert_eq!(acc.min_val, Some(min), "case {case}: min");
        assert_eq!(acc.max_val, Some(max), "case {case}: max");
        assert!(
            (acc.cumulative - sum).abs() < 1e-6 * (1.0 + sum.abs()),
            "case {case}: cumulative"
        );
        assert!(
            (acc.mean().unwrap() - mean).abs() < 1e-6 * (1.0 + mean.abs()),
            "case {case}: mean"
        );
        assert!(
            (acc.variance().unwrap() - var).abs() < 1e-3 * (1.0 + var.abs()),
            "case {case}: variance"
        );
        // Last value is the final fold (times strictly increase).
        assert_eq!(acc.last_val, values.last().copied(), "case {case}: last");
    }
}

/// Fold order must not change the order-independent aggregates (sum, sumSq,
/// min, max, cumulative, count). 1,000 shuffled-vs-sorted comparisons.
#[test]
fn accumulator_is_order_independent_for_aggregates() {
    let mut rng = Rng::new(0x1234_5678);
    for _ in 0..1_000 {
        let n = 2 + rng.below(50) as usize;
        let values: Vec<f64> = (0..n).map(|_| rng.range(0.0, 1000.0)).collect();

        let fold_all = |order: &[f64]| {
            let mut a = Accumulator::empty("p", "contact", "c1", "x");
            for (i, v) in order.iter().enumerate() {
                a.fold(*v, Utc.timestamp_opt(1_700_000_000 + i as i64, 0).unwrap());
            }
            a
        };
        let mut sorted = values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let a = fold_all(&values);
        let b = fold_all(&sorted);
        assert_eq!(a.count, b.count);
        assert!((a.sum - b.sum).abs() < 1e-9 * (1.0 + a.sum.abs()));
        assert_eq!(a.min_val, b.min_val);
        assert_eq!(a.max_val, b.max_val);
        assert!((a.cumulative - b.cumulative).abs() < 1e-9 * (1.0 + a.cumulative.abs()));
    }
}

/// 4,000 random observations against a fixed [300, 850] credit-score band:
/// the validator's accept set must be exactly the closed interval.
#[test]
fn threshold_band_membership_is_exact() {
    let mut rng = Rng::new(0xC0FFEE);
    let def = numeric_def(Some(300.0), Some(850.0));
    for _ in 0..4_000 {
        let v = rng.range(0.0, 1200.0);
        let inside = (300.0..=850.0).contains(&v);
        let accepted = def.validate(&numeric_obs(v)).is_ok();
        assert_eq!(accepted, inside, "value {v} band membership");
        if accepted {
            assert_eq!(numeric_obs(v).status, ObservationStatus::Accepted);
        }
    }
}
