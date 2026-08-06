// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Hybrid scorer: blends three orthogonal relevance signals into a single
//! rankable score.
//!
//!   1. **Lexical** — token-overlap between the query and the memory content
//!      (plus a full-phrase substring bonus). This is the "does it literally
//!      say the words I searched for" signal, and it degrades to exactly the
//!      old substring behavior when there are no embeddings.
//!   2. **Semantic** — cosine similarity between the query embedding and the
//!      memory embedding (supplied by the caller from `vector_search`). This
//!      is the "does it *mean* the same thing" signal — it surfaces
//!      "outdoor trekking" → "hiking in the mountains" even with zero shared
//!      tokens. `None` when semantic search is disabled or the item has no
//!      vector; the term is simply dropped and the remaining weights are
//!      renormalized, so absent semantics never penalize a candidate.
//!   3. **Recency** — an exponential decay on `learned_at`, so fresher
//!      memories win ties. Half-life configurable (default 45 days).
//!
//! The blend is a weighted average over whichever signals are *active* for a
//! given candidate (renormalized by the active weights), so:
//!   - embeddings on, query present  → lexical + semantic + recency
//!   - embeddings off, query present → lexical + recency (no regression vs the
//!     pre-existing substring + newest-first behavior)
//!   - no query (list mode)          → recency only
//!
//! Pure functions, no I/O — the storage layer gathers candidates + calls
//! `rank`; this module never touches a database or an embedding service.

use crate::MemoryItem;
use chrono::{DateTime, Utc};
use std::collections::HashSet;

/// Relative weights for the three signals + the recency decay half-life.
/// Semantic is weighted a touch above lexical so genuine meaning-matches can
/// out-rank incidental token overlap, but lexical stays high enough that an
/// exact keyword hit is never buried.
#[derive(Debug, Clone, Copy)]
pub struct HybridWeights {
    pub lexical: f32,
    pub semantic: f32,
    pub recency: f32,
    /// Days for the recency signal to halve. Larger = recency matters less.
    pub recency_half_life_days: f32,
}

impl Default for HybridWeights {
    fn default() -> Self {
        Self {
            lexical: 0.40,
            semantic: 0.45,
            recency: 0.15,
            recency_half_life_days: 45.0,
        }
    }
}

/// A memory item with its computed component + blended scores. Higher `score`
/// = more relevant. `semantic` is the cosine mapped into `0..=1` (or `0.0`
/// when the item had no vector).
#[derive(Debug, Clone)]
pub struct Scored {
    pub item: MemoryItem,
    pub score: f32,
    pub lexical: f32,
    pub semantic: f32,
    pub recency: f32,
}

/// Split text into lowercase alphanumeric tokens. Shared by the query and
/// content sides so overlap is computed on the same basis.
pub fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Lexical relevance in `0..=1`: fraction of query tokens that appear as
/// tokens in the content, promoted to a full `1.0` when the entire query
/// occurs as a substring (an exact phrase hit is maximally lexical). Returns
/// `0.0` for an empty query.
pub fn lexical_score(query: &str, content: &str) -> f32 {
    let q_tokens = tokenize(query);
    if q_tokens.is_empty() {
        return 0.0;
    }
    let content_lc = content.to_lowercase();
    if content_lc.contains(&query.trim().to_lowercase()) {
        return 1.0;
    }
    let content_tokens: HashSet<String> = tokenize(content).into_iter().collect();
    let hits = q_tokens.iter().filter(|t| content_tokens.contains(*t)).count();
    (hits as f32 / q_tokens.len() as f32).clamp(0.0, 1.0)
}

/// Exponential recency decay in `(0..=1]`: `0.5 ^ (age / half_life)`. An item
/// learned right now scores ~1.0; one half-life old scores 0.5; future
/// timestamps (clock skew) clamp to 1.0.
pub fn recency_score(
    learned_at: DateTime<Utc>,
    now: DateTime<Utc>,
    half_life_days: f32,
) -> f32 {
    let age_days = (now - learned_at).num_seconds() as f32 / 86_400.0;
    if age_days <= 0.0 {
        return 1.0;
    }
    let hl = half_life_days.max(0.001);
    0.5_f32.powf(age_days / hl).clamp(0.0, 1.0)
}

/// Map a raw cosine similarity (`-1..=1`) into the `0..=1` semantic term.
/// Negative similarities (genuinely unrelated on normalized embeddings) carry
/// no positive signal, so they floor at 0.
pub fn semantic_from_cosine(cosine: f32) -> f32 {
    cosine.clamp(0.0, 1.0)
}

/// Blend the active signals for one candidate into a single score. `semantic`
/// is `Some(cosine)` when the item has a comparable vector and semantic search
/// is on, else `None`. `has_query` gates the lexical term (list mode passes
/// `false`). Weights are renormalized over whichever terms are active so a
/// missing signal neither helps nor hurts.
pub fn blend(
    lexical: f32,
    semantic: Option<f32>,
    recency: f32,
    has_query: bool,
    w: &HybridWeights,
) -> f32 {
    let wl = if has_query { w.lexical } else { 0.0 };
    let ws = if semantic.is_some() { w.semantic } else { 0.0 };
    let wr = w.recency;
    let denom = wl + ws + wr;
    if denom <= 0.0 {
        return 0.0;
    }
    let sem = semantic.map(semantic_from_cosine).unwrap_or(0.0);
    (wl * lexical + ws * sem + wr * recency) / denom
}

/// Rank a candidate pool. Each candidate is a `(item, cosine)` pair where
/// `cosine` is `Some` iff the item came back from (or is comparable under) the
/// vector index. `semantic_enabled` reflects whether the active provider
/// produces vectors at all — when `false`, every semantic term is dropped and
/// the blend degrades to lexical + recency. Returns the pool sorted by blended
/// score, highest first (ties broken by recency, then id for determinism).
pub fn rank(
    query: &str,
    now: DateTime<Utc>,
    candidates: Vec<(MemoryItem, Option<f32>)>,
    weights: &HybridWeights,
    semantic_enabled: bool,
) -> Vec<Scored> {
    let has_query = !tokenize(query).is_empty();
    let mut scored: Vec<Scored> = candidates
        .into_iter()
        .map(|(item, cosine)| {
            let lexical = if has_query {
                lexical_score(query, &item.content)
            } else {
                0.0
            };
            let recency = recency_score(item.learned_at, now, weights.recency_half_life_days);
            let semantic_opt = if semantic_enabled { cosine } else { None };
            let score = blend(lexical, semantic_opt, recency, has_query, weights);
            Scored {
                item,
                score,
                lexical,
                semantic: semantic_opt.map(semantic_from_cosine).unwrap_or(0.0),
                recency,
            }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.item.learned_at.cmp(&a.item.learned_at))
            .then(a.item.id.cmp(&b.item.id))
    });
    scored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryScope;

    fn item(id: &str, content: &str, age_days: i64) -> MemoryItem {
        let mut m = MemoryItem::new(id, "local", "fact", content, MemoryScope::Project);
        m.learned_at = Utc::now() - chrono::Duration::days(age_days);
        m
    }

    #[test]
    fn lexical_rewards_overlap_and_phrase() {
        // exact phrase → 1.0
        assert_eq!(lexical_score("postgres database", "we use postgres database here"), 1.0);
        // partial token overlap → fraction
        let s = lexical_score("postgres redis", "we use postgres only");
        assert!((s - 0.5).abs() < 1e-6, "got {s}");
        // no overlap → 0
        assert_eq!(lexical_score("kubernetes", "i enjoy hiking"), 0.0);
        // empty query → 0
        assert_eq!(lexical_score("", "anything"), 0.0);
    }

    #[test]
    fn recency_decays_monotonically() {
        let now = Utc::now();
        let fresh = recency_score(now, now, 45.0);
        let old = recency_score(now - chrono::Duration::days(45), now, 45.0);
        let older = recency_score(now - chrono::Duration::days(90), now, 45.0);
        assert!((fresh - 1.0).abs() < 1e-3);
        assert!((old - 0.5).abs() < 1e-2, "half-life should halve: {old}");
        assert!(older < old && old < fresh);
        // future timestamp clamps to 1.0 rather than exceeding it
        assert_eq!(recency_score(now + chrono::Duration::days(5), now, 45.0), 1.0);
    }

    #[test]
    fn semantic_surfaces_non_lexical_match() {
        // Query shares NO tokens with either item; only the cosine differs.
        let now = Utc::now();
        let hiking = item("a", "i love hiking in the mountains on weekends", 1);
        let budget = item("b", "quarterly budget review meeting notes", 1);
        let ranked = rank(
            "outdoor trekking",
            now,
            vec![(budget, Some(0.15)), (hiking, Some(0.72))],
            &HybridWeights::default(),
            true,
        );
        assert_eq!(ranked[0].item.id, "a", "semantic winner should rank first");
        assert!(ranked[0].score > ranked[1].score);
    }

    #[test]
    fn degrades_to_lexical_plus_recency_when_semantic_off() {
        let now = Utc::now();
        // Lexical match but OLDER; non-match but newer. With semantic off, the
        // lexical hit must still win (it's the whole point of "no regression").
        let hit = item("hit", "we migrated to postgres last year", 200);
        let miss = item("miss", "i had coffee this morning", 0);
        let ranked = rank(
            "postgres",
            now,
            vec![(miss, None), (hit, None)],
            &HybridWeights::default(),
            false,
        );
        assert_eq!(ranked[0].item.id, "hit");
        assert!(ranked[0].semantic == 0.0, "semantic term must be dropped");
    }

    #[test]
    fn no_query_ranks_by_recency() {
        let now = Utc::now();
        let old = item("old", "something", 100);
        let new = item("new", "another thing", 1);
        let ranked = rank("", now, vec![(old, None), (new, None)], &HybridWeights::default(), true);
        assert_eq!(ranked[0].item.id, "new");
    }

    #[test]
    fn absent_semantic_does_not_penalize_the_vectorless_item() {
        // Two identical-content, identical-recency items: one has a vector, one
        // has none. The renormalized blend means the vectorless item is scored
        // purely on the signals it has (lexical + recency) — it must NOT be sunk
        // to zero for lacking a vector; both stay high, near the top.
        let now = Utc::now();
        let a = item("a", "postgres is our database", 1);
        let b = item("b", "postgres is our database", 1);
        let ranked = rank(
            "postgres",
            now,
            vec![(a, Some(0.9)), (b, None)],
            &HybridWeights::default(),
            true,
        );
        let a_scored = ranked.iter().find(|s| s.item.id == "a").unwrap();
        let b_scored = ranked.iter().find(|s| s.item.id == "b").unwrap();
        // The vectorless item keeps a strong score (exact lexical hit + fresh),
        // not a zeroed-out semantic dragging it down.
        assert!(b_scored.score > 0.9, "vectorless item unfairly sunk: {}", b_scored.score);
        assert!(a_scored.score > 0.9, "vectored item also strong: {}", a_scored.score);
    }
}
