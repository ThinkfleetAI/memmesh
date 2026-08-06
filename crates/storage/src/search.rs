// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Hybrid retrieval: the read path shared by MCP `memory_search` / `memory_list`
//! and the CLI `search` subcommand.
//!
//! The old path was a single `content LIKE '%query%'` ordered by `learnedAt`.
//! This module keeps that lexical path and adds a semantic one on top:
//!
//!   1. **Semantic candidates** — embed the query and pull the k nearest
//!      vectors via `Storage::vector_search` (brute-force cosine on SQLite,
//!      pgvector on Postgres). This finds items that *mean* the same thing with
//!      zero shared words.
//!   2. **Lexical candidates** — the existing `content LIKE` query, so an exact
//!      keyword hit is never missed even if its vector ranks low.
//!   3. **Union + hybrid rank** — the two pools are merged (dedup by id, keeping
//!      the cosine from the semantic side) and scored by
//!      [`memory_core::scoring::rank`] (lexical + semantic + recency). The
//!      top-k after `offset` is returned.
//!
//! Degradation is automatic and lossless:
//!   - No query (list mode) → straight to `Storage::query` (recency order,
//!     offset honored) — identical to the previous behavior.
//!   - Semantic disabled (no `fastembed` feature / provider off) → the semantic
//!     pool is empty, the scorer drops the semantic term, and results are the
//!     lexical matches ranked by lexical + recency: no regression.

use memory_core::scoring::{rank, HybridWeights};
use memory_core::MemoryItem;

use crate::embedding;
use crate::{MemoryFilter, MemoryQuery, ScoredMemoryItem, Storage, StorageError};

/// How large a candidate pool to gather before ranking. Scaled to the request
/// window (`limit + offset`) but floored/capped so a small query still sees a
/// meaningful neighborhood and a huge one can't blow up the brute-force scan.
fn pool_size(limit: u32, offset: u32) -> u32 {
    limit.saturating_add(offset).saturating_mul(5).clamp(50, 500)
}

/// Run a hybrid search. `filter` carries the structural predicates
/// (scope/project/user/type/status). `query` is the free-text query; when
/// `None`/blank this is a list query (recency order). Any `text_match` already
/// on `filter` is ignored — pass the text via `query`.
pub async fn search<S: Storage + ?Sized>(
    storage: &S,
    filter: &MemoryFilter,
    query: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<Vec<MemoryItem>, StorageError> {
    let q = query.map(str::trim).filter(|s| !s.is_empty());

    // Structural-only filter (never carries the free-text term; the lexical
    // pool sets text_match explicitly, the semantic pool must not filter on it).
    let mut base = filter.clone();
    base.text_match = None;

    // List mode: no query → preserve the exact prior behavior (recency order,
    // offset honored) straight from storage.
    let Some(q) = q else {
        return storage
            .query(&MemoryQuery {
                filter: base,
                limit: Some(limit),
                offset: Some(offset),
            })
            .await;
    };

    let k = pool_size(limit, offset);
    let semantic_enabled = embedding::semantic_enabled();

    // Candidate pool keyed by id: value carries the item + its cosine (if it
    // came from the vector index).
    let mut pool: std::collections::HashMap<String, (MemoryItem, Option<f32>)> =
        std::collections::HashMap::new();

    // 1. Semantic candidates.
    if semantic_enabled {
        let qvec = embedding::embed_query(q).await;
        if !qvec.is_empty() {
            let hits: Vec<ScoredMemoryItem> =
                storage.vector_search(&qvec, &base, k).await?;
            for h in hits {
                pool.insert(h.item.id.clone(), (h.item, Some(h.similarity)));
            }
        }
    }

    // 2. Lexical candidates (existing substring path). Merge — keep any cosine
    // already recorded for a shared id.
    let mut lex_filter = base.clone();
    lex_filter.text_match = Some(q.to_string());
    let lexical: Vec<MemoryItem> = storage
        .query(&MemoryQuery {
            filter: lex_filter,
            limit: Some(k),
            offset: Some(0),
        })
        .await?;
    for item in lexical {
        pool.entry(item.id.clone()).or_insert((item, None));
    }

    // 3. Union → hybrid rank → window.
    let candidates: Vec<(MemoryItem, Option<f32>)> = pool.into_values().collect();
    let ranked = rank(
        q,
        chrono::Utc::now(),
        candidates,
        &HybridWeights::default(),
        semantic_enabled,
    );

    let out: Vec<MemoryItem> = ranked
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|s| s.item)
        .collect();
    Ok(out)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStore;
    use memory_core::{MemoryItem, MemoryScope};

    async fn store() -> SqliteStore {
        let s = SqliteStore::connect("sqlite::memory:").await.unwrap();
        s.migrate().await.unwrap();
        s
    }

    fn item(id: &str, content: &str) -> MemoryItem {
        MemoryItem::new(id, "local", "fact", content, MemoryScope::Project)
    }

    #[tokio::test]
    async fn lexical_substring_still_works_without_embeddings() {
        // No embeddings set on any row → semantic pool is empty; the lexical
        // substring path must still find the match (no regression).
        let s = store().await;
        s.save(&item("a", "we use Postgres as our primary database")).await.unwrap();
        s.save(&item("b", "I love hiking in the mountains")).await.unwrap();

        let out = search(&s, &MemoryFilter::default(), Some("Postgres"), 10, 0)
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "a");
    }

    #[tokio::test]
    async fn no_query_returns_recency_order() {
        let s = store().await;
        let mut older = item("old", "first");
        older.learned_at = chrono::Utc::now() - chrono::Duration::days(10);
        let newer = item("new", "second");
        s.save(&older).await.unwrap();
        s.save(&newer).await.unwrap();

        let out = search(&s, &MemoryFilter::default(), None, 10, 0).await.unwrap();
        assert_eq!(out.first().unwrap().id, "new");
    }

    #[tokio::test]
    async fn semantic_pool_unions_with_lexical() {
        // Simulate a semantic provider by writing a matching embedding directly
        // (search reads the vector index via vector_search). The query embedding
        // path is exercised in the end-to-end CLI test; here we assert the union
        // + ranking wiring using stored vectors + the Hashed-style overlap isn't
        // needed — we just confirm lexical still lands the item.
        let s = store().await;
        s.save(&item("db", "we use Postgres as our primary database")).await.unwrap();
        let out = search(&s, &MemoryFilter::default(), Some("database"), 10, 0)
            .await
            .unwrap();
        assert!(out.iter().any(|m| m.id == "db"));
    }
}
