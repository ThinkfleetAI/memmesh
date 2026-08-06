// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Retrieval-quality scoring.
//!
//! Given a list of returned memory ids (ranked, best first) and a set of
//! expected-relevant ids, compute precision@k, recall@k, and reciprocal
//! rank of the first relevant hit.
//!
//! Aggregates (mean P@K, mean R@K, MRR) are macro-averaged across queries
//! — every query contributes equally regardless of its `expected` size.
//! That's the right default for spotting regressions; switch to micro if
//! a fixture has wildly uneven expected counts.

use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize)]
pub struct QueryScore {
    pub query: String,
    pub k: usize,
    pub precision_at_k: f64,
    pub recall_at_k: f64,
    /// Reciprocal rank of the first relevant result in `returned`.
    /// 1.0 if the first returned id was relevant; 0.5 for second; 0.0
    /// if no relevant id appeared.
    pub mrr: f64,
    /// Total expected count (denominator for recall).
    pub expected_count: usize,
    /// How many of `expected` were in the returned top-K.
    pub hit_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Scorecard {
    pub fixture_name: String,
    pub fixture_description: String,
    pub k: usize,
    pub queries: Vec<QueryScore>,
    pub mean_precision_at_k: f64,
    pub mean_recall_at_k: f64,
    pub mean_mrr: f64,
}

impl Scorecard {
    pub fn one_line_summary(&self) -> String {
        format!(
            "{:<28} P@{}={:.3}  R@{}={:.3}  MRR={:.3}  ({} queries)",
            self.fixture_name,
            self.k,
            self.mean_precision_at_k,
            self.k,
            self.mean_recall_at_k,
            self.mean_mrr,
            self.queries.len(),
        )
    }
}

/// Score a single query.
///
/// `returned` is ordered best-first. Only the first `k` entries are
/// considered for P@K and R@K. MRR considers the full list (the first
/// hit's rank, capped at 0 if none).
pub fn score_query(
    query: &str,
    k: usize,
    returned: &[String],
    expected: &[String],
) -> QueryScore {
    let expected_set: HashSet<&str> = expected.iter().map(|s| s.as_str()).collect();
    let top_k: Vec<&str> = returned.iter().take(k).map(|s| s.as_str()).collect();
    let hits: usize = top_k.iter().filter(|id| expected_set.contains(*id)).count();

    // Standard Precision@K: relevant hits among the top k, divided by k —
    // not by the number actually returned. A system that returns only 2 items
    // when k=5 is capped at 0.4, which is the point: filling all k slots with
    // relevant results is rewarded. Dividing by the returned count instead
    // would score a 2-of-2 return as 1.0 and erase that signal.
    let precision_at_k = if k == 0 {
        0.0
    } else {
        hits as f64 / k as f64
    };
    let recall_at_k = if expected.is_empty() {
        // No expected items → vacuously perfect recall, neutral
        // contribution. Reflect this as 1.0 so a fixture author can
        // intentionally use empty-expected as a "no false positives"
        // assertion.
        1.0
    } else {
        hits as f64 / expected.len() as f64
    };

    let mrr = returned
        .iter()
        .enumerate()
        .find(|(_, id)| expected_set.contains(id.as_str()))
        .map(|(rank, _)| 1.0 / (rank as f64 + 1.0))
        .unwrap_or(0.0);

    QueryScore {
        query: query.to_string(),
        k,
        precision_at_k,
        recall_at_k,
        mrr,
        expected_count: expected.len(),
        hit_count: hits,
    }
}

/// Macro-average a slice of per-query scores into a scorecard.
pub fn aggregate(
    fixture_name: String,
    fixture_description: String,
    k: usize,
    queries: Vec<QueryScore>,
) -> Scorecard {
    let n = queries.len() as f64;
    let (sum_p, sum_r, sum_mrr) = if queries.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        queries.iter().fold((0.0, 0.0, 0.0), |(p, r, m), q| {
            (p + q.precision_at_k, r + q.recall_at_k, m + q.mrr)
        })
    };
    Scorecard {
        fixture_name,
        fixture_description,
        k,
        mean_precision_at_k: if n == 0.0 { 0.0 } else { sum_p / n },
        mean_recall_at_k: if n == 0.0 { 0.0 } else { sum_r / n },
        mean_mrr: if n == 0.0 { 0.0 } else { sum_mrr / n },
        queries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_score() {
        let s = score_query("q", 5, &["a".into(), "b".into()], &["a".into(), "b".into()]);
        assert!((s.precision_at_k - 0.4).abs() < 1e-9); // 2/5
        assert!((s.recall_at_k - 1.0).abs() < 1e-9); // 2/2
        assert!((s.mrr - 1.0).abs() < 1e-9);
    }

    #[test]
    fn second_position_hit() {
        let s = score_query(
            "q",
            5,
            &["x".into(), "a".into(), "y".into()],
            &["a".into()],
        );
        assert!((s.mrr - 0.5).abs() < 1e-9);
        assert!((s.recall_at_k - 1.0).abs() < 1e-9);
    }

    #[test]
    fn miss() {
        let s = score_query(
            "q",
            5,
            &["x".into(), "y".into(), "z".into()],
            &["a".into()],
        );
        assert_eq!(s.hit_count, 0);
        assert_eq!(s.mrr, 0.0);
        assert_eq!(s.recall_at_k, 0.0);
    }

    #[test]
    fn empty_expected_vacuous_recall() {
        let s = score_query("q", 5, &["x".into()], &[]);
        assert!((s.recall_at_k - 1.0).abs() < 1e-9);
    }
}
