// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Embedding provider abstraction.
//!
//! Vectors are a **local, derived index** in this engine: each storage
//! backend regenerates them from `content` with its own configured provider
//! and never syncs them across the SaaS boundary. That means callers only
//! ever compare vectors produced by the *same* provider in the *same* store,
//! so the only hard contract here is "give me a deterministic vector of a
//! fixed dimension for this text".
//!
//! Providers:
//!   - [`NoneEmbedder`]   — disables semantic search (zero-length vectors).
//!     The honest default until a real model is configured: the hybrid scorer
//!     simply drops the semantic term and degrades to lexical + recency.
//!   - [`HashedEmbedder`] — dependency-free, deterministic hashing into a
//!     fixed-dim space. **Not semantic** — it exists so the storage + scorer
//!     wiring can be built and tested end-to-end without an ONNX runtime, and
//!     as a stable test double. Never select it in production.
//!
//!   - `FastEmbedder` (feature `fastembed`) — the real local ONNX model,
//!     in-process, no API key, no network, no data egress (which is what makes
//!     it safe for patient data). Default model `bge-large-en-v1.5` (1024-dim);
//!     `bge-small-en-v1.5` (384-dim) also supported. Built without the feature,
//!     [`EmbeddingConfig::Local`] degrades to [`NoneEmbedder`] rather than
//!     failing to start.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Which embedding backend to construct. Maps 1:1 to the `[embeddings]`
/// config section the engine reads at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "provider")]
pub enum EmbeddingConfig {
    /// Semantic search disabled. Scorer uses lexical + recency only.
    None,
    /// Deterministic hashing into `dim` floats. Test/dev only.
    Hashed { dim: usize },
    /// Local ONNX model. `model` is a fastembed model id; defaults applied
    /// when omitted. Only honored when built with the `fastembed` feature.
    Local { model: Option<String> },
    /// Remote embedding service (our embed-server, or any compatible `/embed`
    /// endpoint) — e.g. bge-large running on a dedicated box. `dim` MUST match
    /// the model the service runs and the vector column (bge-large = 1024).
    /// Only honored when built with the `remote` feature.
    Remote {
        url: String,
        model: Option<String>,
        #[serde(default = "default_remote_dim")]
        dim: usize,
    },
}

fn default_remote_dim() -> usize {
    1024 // bge-large-en-v1.5
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        // Honest default: no semantic search until a model is configured.
        // Flipping the default to `Local` is a one-line change once the
        // fastembed runtime is part of the standard build.
        EmbeddingConfig::None
    }
}

/// A source of text embeddings. Implementations must be deterministic for a
/// given input (so re-embedding the same `content` yields a comparable
/// vector) and must return vectors of exactly [`dim`](EmbeddingProvider::dim)
/// length — except [`NoneEmbedder`], whose `dim` is 0.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync + 'static {
    /// Embed a batch of texts. Returns one vector per input, in order.
    /// Batched because real models amortize far better over a batch than
    /// per-call, and the storage backfill path embeds in bulk.
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>>;

    /// Dimensionality of the produced vectors. `0` means "no embeddings"
    /// (the [`NoneEmbedder`]); callers use this to size vector columns and
    /// to decide whether to run the semantic path at all.
    fn dim(&self) -> usize;

    /// Stable identifier for the active model, e.g. `bge-small-en-v1.5`.
    /// Persisted alongside vectors so a model change can invalidate the
    /// derived index and trigger a re-embed rather than silently comparing
    /// incompatible vectors.
    fn model_id(&self) -> &str;

    /// Convenience: embed a single text.
    async fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut out = self.embed(std::slice::from_ref(&text.to_owned())).await?;
        Ok(out.pop().unwrap_or_default())
    }

    /// Whether this provider produces usable vectors. `false` for
    /// [`NoneEmbedder`]; lets the query path skip the ANN entirely.
    fn is_enabled(&self) -> bool {
        self.dim() > 0
    }
}

/// Build the provider described by `config`. Returns a boxed trait object so
/// the rest of the engine holds one type regardless of backend.
///
/// `EmbeddingConfig::Local` builds the real ONNX model when compiled with the
/// `fastembed` feature; without that feature it falls back to [`NoneEmbedder`]
/// with a warning, so a config that asks for a model degrades safely instead of
/// refusing to start.
pub fn build_provider(config: &EmbeddingConfig) -> Box<dyn EmbeddingProvider> {
    match config {
        EmbeddingConfig::None => Box::new(NoneEmbedder),
        EmbeddingConfig::Hashed { dim } => Box::new(HashedEmbedder::new(*dim)),
        EmbeddingConfig::Local { model } => build_local(model.as_deref()),
        EmbeddingConfig::Remote { url, model, dim } => build_remote(url, model.as_deref(), *dim),
    }
}

#[cfg(feature = "remote")]
fn build_remote(url: &str, model: Option<&str>, dim: usize) -> Box<dyn EmbeddingProvider> {
    Box::new(remote::RemoteEmbedder::new(url, model, dim))
}

#[cfg(not(feature = "remote"))]
fn build_remote(_url: &str, _model: Option<&str>, _dim: usize) -> Box<dyn EmbeddingProvider> {
    tracing::warn!(
        "EmbeddingConfig::Remote requested but built without the `remote` \
         feature; semantic search disabled (NoneEmbedder)"
    );
    Box::new(NoneEmbedder)
}

#[cfg(feature = "fastembed")]
fn build_local(model: Option<&str>) -> Box<dyn EmbeddingProvider> {
    match fast::FastEmbedder::new(model) {
        Ok(e) => Box::new(e),
        Err(err) => {
            tracing::error!(?err, "fastembed init failed; semantic search disabled");
            Box::new(NoneEmbedder)
        }
    }
}

#[cfg(not(feature = "fastembed"))]
fn build_local(_model: Option<&str>) -> Box<dyn EmbeddingProvider> {
    tracing::warn!(
        "EmbeddingConfig::Local requested but built without the `fastembed` \
         feature; semantic search disabled (NoneEmbedder)"
    );
    Box::new(NoneEmbedder)
}

// ─── Cosine similarity ──────────────────────────────────────────────

/// Cosine similarity in `-1..=1`, or `0.0` when either vector is empty or
/// zero-norm (degenerate inputs contribute no signal rather than NaN). The
/// scorer maps this into its semantic term.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

// ─── NoneEmbedder ───────────────────────────────────────────────────

/// Disables semantic search. `embed` returns empty vectors; `dim` is 0.
pub struct NoneEmbedder;

#[async_trait]
impl EmbeddingProvider for NoneEmbedder {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(vec![Vec::new(); texts.len()])
    }
    fn dim(&self) -> usize {
        0
    }
    fn model_id(&self) -> &str {
        "none"
    }
}

// ─── HashedEmbedder ─────────────────────────────────────────────────

/// Deterministic, dependency-free embedder for build/test wiring. Hashes
/// whitespace tokens into a fixed-dim bag-of-words vector and L2-normalizes
/// it. Captures lexical overlap (shared tokens → higher cosine) but **no**
/// real semantics — synonyms don't move together. Production must use a real
/// model; this only exists so everything downstream can be exercised without
/// an ONNX runtime.
pub struct HashedEmbedder {
    dim: usize,
}

impl HashedEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }

    fn embed_text(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dim];
        for token in text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()) {
            let h = fnv1a(token.to_lowercase().as_bytes());
            let idx = (h as usize) % self.dim;
            v[idx] += 1.0;
        }
        l2_normalize(&mut v);
        v
    }
}

#[async_trait]
impl EmbeddingProvider for HashedEmbedder {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.embed_text(t)).collect())
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn model_id(&self) -> &str {
        "hashed-bow-v1"
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

// ─── FastEmbedder (feature = "fastembed") ───────────────────────────

#[cfg(feature = "fastembed")]
mod fast {
    use super::*;
    use std::sync::Mutex;

    use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

    /// Default local model: bge-large-en-v1.5 (1024-dim) — the project's chosen
    /// model, run on a dedicated CPU server.
    const DEFAULT_MODEL_ID: &str = "bge-large-en-v1.5";

    pub struct FastEmbedder {
        // fastembed's `embed` takes `&mut self`; wrap in a Mutex so the provider
        // stays `Sync` behind the shared trait object.
        model: Mutex<TextEmbedding>,
        dim: usize,
        model_id: String,
    }

    impl FastEmbedder {
        pub fn new(model: Option<&str>) -> anyhow::Result<Self> {
            let model_id = model.unwrap_or(DEFAULT_MODEL_ID).to_string();
            let (embedding_model, dim) = resolve_model(&model_id)?;
            let model = TextEmbedding::try_new(
                InitOptions::new(embedding_model).with_show_download_progress(false),
            )?;
            Ok(Self { model: Mutex::new(model), dim, model_id })
        }
    }

    /// Map a model id to its fastembed enum + output dimension. Dimension is
    /// explicit so a model swap forces a conscious update rather than silently
    /// mis-sizing the vector column.
    fn resolve_model(id: &str) -> anyhow::Result<(EmbeddingModel, usize)> {
        match id {
            "bge-large-en-v1.5" => Ok((EmbeddingModel::BGELargeENV15, 1024)),
            "bge-small-en-v1.5" => Ok((EmbeddingModel::BGESmallENV15, 384)),
            other => anyhow::bail!("unsupported fastembed model id: {other}"),
        }
    }

    #[async_trait]
    impl EmbeddingProvider for FastEmbedder {
        async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            let docs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            let guard = self
                .model
                .lock()
                .map_err(|_| anyhow::anyhow!("embedding model mutex poisoned"))?;
            let out = guard.embed(docs, None)?;
            Ok(out)
        }
        fn dim(&self) -> usize {
            self.dim
        }
        fn model_id(&self) -> &str {
            &self.model_id
        }
    }
}

// ─── RemoteEmbedder (feature = "remote") ────────────────────────────

#[cfg(feature = "remote")]
mod remote {
    use super::*;
    use serde::Deserialize;

    /// Calls a remote embedding service (our `embed-server`) over HTTP. Lets
    /// the heavy bge-large model run on a dedicated box while the engine pod
    /// stays light — the engine just POSTs text and gets vectors back. `dim` is
    /// configured (not discovered) so the trait's `dim()` stays sync and the
    /// vector column can be sized without a startup round-trip.
    pub struct RemoteEmbedder {
        client: reqwest::Client,
        embed_url: String,
        dim: usize,
        model_id: String,
    }

    #[derive(serde::Serialize)]
    struct EmbedRequest<'a> {
        texts: &'a [String],
    }

    #[derive(Deserialize)]
    struct EmbedResponse {
        vectors: Vec<Vec<f32>>,
    }

    impl RemoteEmbedder {
        pub fn new(url: &str, model: Option<&str>, dim: usize) -> Self {
            Self {
                client: reqwest::Client::new(),
                embed_url: format!("{}/embed", url.trim_end_matches('/')),
                dim,
                model_id: model.unwrap_or("remote").to_string(),
            }
        }
    }

    #[async_trait]
    impl EmbeddingProvider for RemoteEmbedder {
        async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }
            let resp = self
                .client
                .post(&self.embed_url)
                .json(&EmbedRequest { texts })
                .send()
                .await?
                .error_for_status()?
                .json::<EmbedResponse>()
                .await?;
            Ok(resp.vectors)
        }
        fn dim(&self) -> usize {
            self.dim
        }
        fn model_id(&self) -> &str {
            &self.model_id
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn none_embedder_is_disabled() {
        let e = NoneEmbedder;
        assert_eq!(e.dim(), 0);
        assert!(!e.is_enabled());
        let out = e.embed(&["hello".into(), "world".into()]).await.unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|v| v.is_empty()));
    }

    #[tokio::test]
    async fn hashed_embedder_is_deterministic_and_sized() {
        let e = HashedEmbedder::new(64);
        assert_eq!(e.dim(), 64);
        assert!(e.is_enabled());
        let a = e.embed_one("weekly coffee order").await.unwrap();
        let b = e.embed_one("weekly coffee order").await.unwrap();
        assert_eq!(a, b, "same text must embed identically");
        assert_eq!(a.len(), 64);
    }

    #[tokio::test]
    async fn hashed_overlap_scores_higher_than_disjoint() {
        let e = HashedEmbedder::new(256);
        let q = e.embed_one("weekly coffee order").await.unwrap();
        let related = e.embed_one("the weekly coffee order is large").await.unwrap();
        let unrelated = e.embed_one("quarterly budget review meeting").await.unwrap();
        let s_related = cosine_similarity(&q, &related);
        let s_unrelated = cosine_similarity(&q, &unrelated);
        assert!(
            s_related > s_unrelated,
            "shared tokens ({s_related}) should beat disjoint ({s_unrelated})"
        );
    }

    #[test]
    fn cosine_handles_degenerate_inputs() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        let v = vec![1.0f32, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn config_default_is_none() {
        assert_eq!(EmbeddingConfig::default(), EmbeddingConfig::None);
    }

    #[test]
    fn build_provider_constructs_each_variant() {
        assert_eq!(build_provider(&EmbeddingConfig::None).dim(), 0);
        assert_eq!(build_provider(&EmbeddingConfig::Hashed { dim: 32 }).dim(), 32);
        // Local without the feature degrades to None (dim 0), not a panic.
        let local = build_provider(&EmbeddingConfig::Local { model: None });
        let _ = local.dim();
    }

    #[test]
    fn config_round_trips_through_json() {
        let c = EmbeddingConfig::Local { model: Some("bge-small-en-v1.5".into()) };
        let s = serde_json::to_string(&c).unwrap();
        let back: EmbeddingConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn remote_config_round_trips_and_defaults_dim() {
        let c = EmbeddingConfig::Remote {
            url: "http://embed:80".into(),
            model: Some("bge-large-en-v1.5".into()),
            dim: 1024,
        };
        let back: EmbeddingConfig = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(c, back);
        // `dim` defaults to bge-large's 1024 when omitted.
        let defaulted: EmbeddingConfig =
            serde_json::from_str(r#"{"provider":"remote","url":"http://x"}"#).unwrap();
        assert_eq!(
            defaulted,
            EmbeddingConfig::Remote { url: "http://x".into(), model: None, dim: 1024 }
        );
    }

    #[test]
    fn build_provider_remote_respects_feature() {
        let p = build_provider(&EmbeddingConfig::Remote {
            url: "http://x".into(),
            model: None,
            dim: 1024,
        });
        // Without the `remote` feature it degrades to None (dim 0); with it, the
        // configured dim is reported.
        #[cfg(not(feature = "remote"))]
        assert_eq!(p.dim(), 0);
        #[cfg(feature = "remote")]
        assert_eq!(p.dim(), 1024);
    }
}
