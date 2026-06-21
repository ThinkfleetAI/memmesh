// Dedicated embedding service. Hosts the heavy local model (bge-large via
// fastembed/ONNX) behind a tiny HTTP API so the engine pod can run light and
// call it through the `remote` EmbeddingProvider. One model implementation
// (memory_embed::FastEmbedder), served either in-process or here.
//
// Wire format (matches RemoteEmbedder):
//   POST /embed   {"texts": ["a","b"]} -> {"model","dim","vectors":[[...],[...]]}
//   GET  /health  -> {"ok": true, "model", "dim"}
//
// Env:
//   EMBED_MODEL  model id (default "bge-large-en-v1.5")
//   EMBED_ADDR   bind address (default "0.0.0.0:8080")

use std::sync::Arc;

use axum::{extract::State, routing::{get, post}, Json, Router};
use memory_embed::{build_provider, EmbeddingConfig, EmbeddingProvider};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct AppState {
    provider: Arc<dyn EmbeddingProvider>,
    model: String,
}

#[derive(Deserialize)]
struct EmbedRequest {
    texts: Vec<String>,
}

#[derive(Serialize)]
struct EmbedResponse {
    model: String,
    dim: usize,
    vectors: Vec<Vec<f32>>,
}

#[derive(Serialize)]
struct Health {
    ok: bool,
    model: String,
    dim: usize,
}

async fn health(State(s): State<AppState>) -> Json<Health> {
    Json(Health { ok: true, model: s.model.clone(), dim: s.provider.dim() })
}

async fn embed(
    State(s): State<AppState>,
    Json(req): Json<EmbedRequest>,
) -> Result<Json<EmbedResponse>, (axum::http::StatusCode, String)> {
    let vectors = s
        .provider
        .embed(&req.texts)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(EmbedResponse { model: s.model.clone(), dim: s.provider.dim(), vectors }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let model = std::env::var("EMBED_MODEL").unwrap_or_else(|_| "bge-large-en-v1.5".to_string());
    let addr = std::env::var("EMBED_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    // Build (and download/load) the model once at startup.
    let provider: Arc<dyn EmbeddingProvider> =
        Arc::from(build_provider(&EmbeddingConfig::Local { model: Some(model.clone()) }));
    if !provider.is_enabled() {
        anyhow::bail!(
            "embedding provider is disabled — embed-server must be built with the \
             memory-embed `fastembed` feature so the model loads"
        );
    }
    tracing::info!(model = %provider.model_id(), dim = provider.dim(), %addr, "embed-server ready");

    let state = AppState { provider, model };
    let app = Router::new()
        .route("/health", get(health))
        .route("/embed", post(embed))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
