// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Basic memory consolidation: find near-duplicate memories within a scope and
//! non-destructively fold the redundant ones into a survivor.
//!
//! "Near-duplicate" = cosine similarity of the two items' embeddings `>=`
//! `threshold` (default 0.95). Redundant items are collapsed via the existing
//! [`Storage::supersede`] path — the loser is marked `superseded` and pointed
//! at the survivor, so nothing is deleted and full provenance is kept.
//!
//! Survivor selection: within a matched pair the higher-importance item wins;
//! ties break to the older (earlier `learned_at`) item — the more established
//! memory absorbs the newer echo.
//!
//! Safety + idempotency:
//!   - Only `confirmed` items with no existing `supersededById` are considered,
//!     so a second run skips everything already collapsed — running it twice is
//!     a no-op.
//!   - `dry_run` reports what *would* happen without writing.
//!   - When semantic embeddings are unavailable (built without `fastembed`, or
//!     provider off), it falls back to exact normalized-text equality so it
//!     still de-dupes safely rather than doing nothing surprising.

use memory_core::{MemoryItem, MemoryStatus};
use memory_embed::cosine_similarity;

use crate::embedding;
use crate::{MemoryFilter, MemoryQuery, Storage, StorageError};

/// Upper bound on how many candidate rows a single run considers. The pairwise
/// comparison is O(n²); this keeps a runaway scope bounded on local hardware.
const MAX_CANDIDATES: u32 = 5_000;

/// One collapse: `loser` was superseded by `survivor` at `similarity`.
#[derive(Debug, Clone)]
pub struct Collapse {
    pub loser_id: String,
    pub survivor_id: String,
    pub similarity: f32,
}

/// Outcome of a consolidation run.
#[derive(Debug, Clone)]
pub struct ConsolidateReport {
    pub scanned: usize,
    pub threshold: f32,
    pub semantic: bool,
    pub dry_run: bool,
    pub collapses: Vec<Collapse>,
}

/// Rank a survivor ahead of the items it absorbs: higher importance first,
/// then older (earlier `learned_at`) first.
fn survivor_rank(a: &MemoryItem, b: &MemoryItem) -> std::cmp::Ordering {
    b.importance
        .partial_cmp(&a.importance)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(a.learned_at.cmp(&b.learned_at))
        .then(a.id.cmp(&b.id))
}

fn normalized(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Find and (unless `dry_run`) collapse near-duplicates in `filter`'s scope.
pub async fn consolidate<S: Storage + ?Sized>(
    storage: &S,
    filter: &MemoryFilter,
    threshold: f32,
    dry_run: bool,
) -> Result<ConsolidateReport, StorageError> {
    // Only live, not-yet-superseded items — this is what makes re-runs a no-op.
    let mut base = filter.clone();
    base.text_match = None;
    base.status = Some(MemoryStatus::Confirmed);

    let mut items: Vec<MemoryItem> = storage
        .query(&MemoryQuery {
            filter: base,
            limit: Some(MAX_CANDIDATES),
            offset: Some(0),
        })
        .await?;
    // Defensively drop anything already pointing at a survivor.
    items.retain(|m| m.superseded_by_id.is_none());
    let scanned = items.len();

    // Order so the preferred survivor is visited before its duplicates.
    items.sort_by(survivor_rank);

    let semantic = embedding::semantic_enabled();
    // Embed every candidate once (batch) if semantic search is on.
    let vectors: Vec<Vec<f32>> = if semantic {
        let provider = embedding::provider();
        let texts: Vec<String> = items.iter().map(|m| m.content.clone()).collect();
        match provider.embed(&texts).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "consolidate: batch embed failed; using text equality");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let have_vectors = vectors.len() == items.len() && !vectors.is_empty();

    let n = items.len();
    let mut absorbed = vec![false; n];
    let mut collapses: Vec<Collapse> = Vec::new();

    for i in 0..n {
        if absorbed[i] {
            continue;
        }
        for j in (i + 1)..n {
            if absorbed[j] {
                continue;
            }
            let sim = if have_vectors {
                cosine_similarity(&vectors[i], &vectors[j])
            } else if normalized(&items[i].content) == normalized(&items[j].content) {
                1.0
            } else {
                0.0
            };
            if sim >= threshold {
                absorbed[j] = true;
                collapses.push(Collapse {
                    loser_id: items[j].id.clone(),
                    survivor_id: items[i].id.clone(),
                    similarity: sim,
                });
            }
        }
    }

    if !dry_run {
        for c in &collapses {
            // Reuse the audited supersede path so provenance is preserved.
            if let Err(e) = storage.supersede(&c.loser_id, &c.survivor_id).await {
                tracing::warn!(
                    loser = %c.loser_id,
                    survivor = %c.survivor_id,
                    error = %e,
                    "consolidate: supersede failed"
                );
            }
        }
    }

    Ok(ConsolidateReport {
        scanned,
        threshold,
        semantic: have_vectors,
        dry_run,
        collapses,
    })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStore;
    use memory_core::MemoryScope;

    async fn store() -> SqliteStore {
        let s = SqliteStore::connect("sqlite::memory:").await.unwrap();
        s.migrate().await.unwrap();
        s
    }

    fn item(id: &str, content: &str, importance: f32) -> MemoryItem {
        let mut m = MemoryItem::new(id, "local", "fact", content, MemoryScope::Project);
        m.importance = importance;
        m
    }

    #[tokio::test]
    async fn text_equal_duplicates_collapse_into_higher_importance_survivor() {
        // No embeddings in the test build path guaranteed → exercise the
        // text-equality fallback deterministically.
        let s = store().await;
        s.save(&item("dup_lo", "We use Postgres  as   our database", 3.0)).await.unwrap();
        s.save(&item("dup_hi", "we use postgres as our database", 8.0)).await.unwrap();
        s.save(&item("other", "I enjoy hiking on weekends", 5.0)).await.unwrap();

        let report = consolidate(&s, &MemoryFilter::default(), 0.95, false)
            .await
            .unwrap();

        assert_eq!(report.scanned, 3);
        assert_eq!(report.collapses.len(), 1, "one duplicate pair");
        let c = &report.collapses[0];
        assert_eq!(c.survivor_id, "dup_hi", "higher importance survives");
        assert_eq!(c.loser_id, "dup_lo");

        // Loser is superseded and points at the survivor.
        let loser = s.get("dup_lo").await.unwrap().unwrap();
        assert_eq!(loser.status, MemoryStatus::Superseded);
        assert_eq!(loser.superseded_by_id.as_deref(), Some("dup_hi"));
        // Survivor + unrelated item untouched.
        assert_eq!(s.get("dup_hi").await.unwrap().unwrap().status, MemoryStatus::Confirmed);
        assert_eq!(s.get("other").await.unwrap().unwrap().status, MemoryStatus::Confirmed);
    }

    #[tokio::test]
    async fn consolidate_is_idempotent() {
        let s = store().await;
        s.save(&item("a", "same text here", 5.0)).await.unwrap();
        s.save(&item("b", "same text here", 5.0)).await.unwrap();

        let first = consolidate(&s, &MemoryFilter::default(), 0.95, false).await.unwrap();
        assert_eq!(first.collapses.len(), 1);
        // Second run: the loser is already superseded → excluded → nothing to do.
        let second = consolidate(&s, &MemoryFilter::default(), 0.95, false).await.unwrap();
        assert_eq!(second.collapses.len(), 0);
        assert_eq!(second.scanned, 1);
    }

    #[tokio::test]
    async fn dry_run_reports_without_writing() {
        let s = store().await;
        s.save(&item("a", "same text here", 5.0)).await.unwrap();
        s.save(&item("b", "same text here", 5.0)).await.unwrap();

        let report = consolidate(&s, &MemoryFilter::default(), 0.95, true).await.unwrap();
        assert_eq!(report.collapses.len(), 1);
        // Nothing actually superseded.
        assert_eq!(s.get("a").await.unwrap().unwrap().status, MemoryStatus::Confirmed);
        assert_eq!(s.get("b").await.unwrap().unwrap().status, MemoryStatus::Confirmed);
    }
}
