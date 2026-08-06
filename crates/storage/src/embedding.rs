// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Process-wide embedding provider + write-path helper.
//!
//! The provider is built once per process from the engine config
//! (`[embeddings]` / `THINKFLEET_EMBEDDINGS_*`) and cached — constructing a
//! `FastEmbedder` loads an ONNX model, which we never want to pay per call.
//! Everything that needs to embed (the write path, query embedding, and
//! consolidation) goes through [`provider`] so they all share one loaded model
//! and one config resolution.
//!
//! When built without the `fastembed` feature — or when the model fails to
//! load — `build_provider` returns a `NoneEmbedder`, so `provider().is_enabled()`
//! is `false` and every caller degrades to lexical + recency automatically.

use std::sync::Arc;

use memory_embed::{build_provider, EmbeddingProvider};
use once_cell::sync::OnceCell;

use crate::{Storage, StorageError};

static PROVIDER: OnceCell<Arc<dyn EmbeddingProvider>> = OnceCell::new();

/// The shared embedding provider for this process. Built lazily from the
/// resolved engine config on first use, then reused. Cheap to clone (`Arc`).
pub fn provider() -> Arc<dyn EmbeddingProvider> {
    PROVIDER
        .get_or_init(|| {
            let cfg = memory_core::config::Config::load_or_default();
            let p: Arc<dyn EmbeddingProvider> = Arc::from(build_provider(&cfg.embeddings));
            if p.is_enabled() {
                tracing::info!(
                    model = p.model_id(),
                    dim = p.dim(),
                    "semantic search enabled (local embeddings)"
                );
            } else {
                tracing::info!(
                    "semantic search disabled; ranking is lexical + recency only"
                );
            }
            p
        })
        .clone()
}

/// True iff the active provider produces usable vectors.
pub fn semantic_enabled() -> bool {
    provider().is_enabled()
}

/// Generate and store the embedding for a freshly-saved item. No-op (cleanly)
/// when the provider is disabled or produces an empty vector. Embedding
/// failures are logged, never fatal — the memory is already persisted; the
/// vector is a derived index that can be backfilled later.
///
/// Uses `set_embedding`, which writes only the `embedding` column and does NOT
/// bump `updated`, so re-embedding never masquerades as a content change to
/// the sync loop.
pub async fn embed_and_store<S: Storage + ?Sized>(storage: &S, id: &str, content: &str) {
    let p = provider();
    if !p.is_enabled() || content.trim().is_empty() {
        return;
    }
    match p.embed_one(content).await {
        Ok(v) if !v.is_empty() => {
            if let Err(e) = storage.set_embedding(id, &v).await {
                tracing::warn!(memory_id = %id, error = %e, "failed to store embedding");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(memory_id = %id, error = %e, "embedding generation failed"),
    }
}

/// Embed the query text with the shared provider. Returns an empty vector when
/// semantic search is off, so callers can treat "no vector" and "disabled"
/// uniformly. Errors degrade to an empty vector (lexical-only fallback).
pub async fn embed_query(text: &str) -> Vec<f32> {
    let p = provider();
    if !p.is_enabled() || text.trim().is_empty() {
        return Vec::new();
    }
    match p.embed_one(text).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "query embedding failed; falling back to lexical");
            Vec::new()
        }
    }
}

/// Convenience for callers that already hold a `MemoryItem`-ish id/content and
/// want a `Result`-shaped variant (used by explicit backfill paths).
pub async fn embed_and_store_checked<S: Storage + ?Sized>(
    storage: &S,
    id: &str,
    content: &str,
) -> Result<(), StorageError> {
    let p = provider();
    if !p.is_enabled() || content.trim().is_empty() {
        return Ok(());
    }
    let v = p
        .embed_one(content)
        .await
        .map_err(|e| StorageError::Other(anyhow::anyhow!(e)))?;
    if !v.is_empty() {
        storage.set_embedding(id, &v).await?;
    }
    Ok(())
}
