// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! HTTP REST API for the GUI (thinkfleet-desktop).
//!
//! Bound to loopback by default — this is a local-only management API,
//! not an internet-facing service. The GUI process talks to it via
//! `127.0.0.1:<port>`.
//!
//! Endpoints:
//!
//!   GET    /health                          — liveness + version + counts
//!   GET    /memory/:id                      — fetch one memory item
//!   POST   /memory                          — upsert
//!   DELETE /memory/:id                      — soft delete (status=rejected)
//!   DELETE /memory/:id?hard=true            — physical delete
//!   POST   /search                          — body = MemoryFilter+limit
//!   POST   /memory/:id/touch                — bump lastAccessedAt
//!   POST   /memory/:id/supersede            — body = { byId }
//!
//! Response shape is JSON; errors carry a stable `code` field for the GUI
//! to switch on. No auth in v1 — loopback only. Token auth lands when the
//! API gets exposed beyond localhost.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{delete, get, post},
    Router,
};
use memory_core::MemoryItem;
use memory_storage::{
    observe::{ObserveRequest, ObserveResponse},
    MemoryFilter, MemoryQuery, Storage, StorageError,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

struct AppState<S: Storage> {
    storage: Arc<S>,
}

// Manual Clone — derive(Clone) on a generic struct adds `S: Clone` bound,
// which we don't want (Storage is behind an Arc).
impl<S: Storage> Clone for AppState<S> {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

fn err(
    code: &'static str,
    status: StatusCode,
    message: impl Into<String>,
) -> axum::response::Response {
    (
        status,
        Json(ErrorBody {
            code,
            message: message.into(),
        }),
    )
        .into_response()
}

fn map_err(e: StorageError) -> axum::response::Response {
    match e {
        StorageError::NotFound(s) => err("not_found", StatusCode::NOT_FOUND, s),
        StorageError::SchemaMismatch(s) => {
            err("schema_mismatch", StatusCode::INTERNAL_SERVER_ERROR, s)
        }
        StorageError::Conflict(s) => err("conflict", StatusCode::CONFLICT, s),
        StorageError::Unsupported(s) => err("unsupported", StatusCode::NOT_IMPLEMENTED, s),
        StorageError::Sqlx(e) => err("sqlx", StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        StorageError::Other(e) => err("other", StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── Health ──────────────────────────────────────────────────

#[derive(Serialize)]
struct HealthBody {
    name: &'static str,
    version: &'static str,
    backend: &'static str,
}

async fn health<S: Storage>(State(s): State<AppState<S>>) -> Json<HealthBody> {
    Json(HealthBody {
        name: "memmesh",
        version: env!("CARGO_PKG_VERSION"),
        backend: s.storage.backend(),
    })
}

// ── /memory/:id ─────────────────────────────────────────────

async fn get_memory<S: Storage>(
    State(s): State<AppState<S>>,
    Path(id): Path<String>,
) -> axum::response::Response {
    match s.storage.get(&id).await {
        Ok(Some(item)) => {
            // Bump lastAccessedAt on read; ignore the error (it just
            // means the row was deleted between get and touch — rare).
            let _ = s.storage.touch(&id).await;
            Json(item).into_response()
        }
        Ok(None) => err("not_found", StatusCode::NOT_FOUND, id),
        Err(e) => map_err(e),
    }
}

async fn save_memory<S: Storage>(
    State(s): State<AppState<S>>,
    Json(item): Json<MemoryItem>,
) -> axum::response::Response {
    let cap = memory_storage::quota::effective_cap(
        &memory_core::config::Config::load_or_default(),
    );
    if let Err(e) = memory_storage::quota::ensure_under_cap(&*s.storage, cap).await {
        return err("quota_exceeded", StatusCode::PAYMENT_REQUIRED, &e.to_string());
    }
    match s.storage.save(&item).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "id": item.id }))).into_response(),
        Err(e) => map_err(e),
    }
}

#[derive(Deserialize)]
struct DeleteQuery {
    #[serde(default)]
    hard: bool,
}

async fn delete_memory<S: Storage>(
    State(s): State<AppState<S>>,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> axum::response::Response {
    match s.storage.delete(&id, q.hard).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "id": id, "hard": q.hard })),
        )
            .into_response(),
        Err(e) => map_err(e),
    }
}

// ── /search ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchBody {
    #[serde(flatten, default)]
    filter: MemoryFilter,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
}

async fn search_memory<S: Storage>(
    State(s): State<AppState<S>>,
    Json(body): Json<SearchBody>,
) -> axum::response::Response {
    match s
        .storage
        .query(&MemoryQuery {
            filter: body.filter,
            limit: body.limit.or(Some(50)),
            offset: body.offset,
        })
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => map_err(e),
    }
}

// ── /observe ────────────────────────────────────────────────

async fn observe_handler<S: Storage>(
    State(s): State<AppState<S>>,
    Json(req): Json<ObserveRequest>,
) -> axum::response::Response {
    match memory_storage::observe::observe(s.storage.as_ref(), &req).await {
        Ok(resp) => Json::<ObserveResponse>(resp).into_response(),
        Err(e) => map_err(e),
    }
}

// ── /memory/:id/{touch,supersede} ───────────────────────────

async fn touch_memory<S: Storage>(
    State(s): State<AppState<S>>,
    Path(id): Path<String>,
) -> axum::response::Response {
    match s.storage.touch(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => map_err(e),
    }
}

#[derive(Deserialize)]
struct SupersedeBody {
    by_id: String,
}

async fn supersede_memory<S: Storage>(
    State(s): State<AppState<S>>,
    Path(id): Path<String>,
    Json(body): Json<SupersedeBody>,
) -> axum::response::Response {
    match s.storage.supersede(&id, &body.by_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => map_err(e),
    }
}

// ── Public entrypoint ───────────────────────────────────────

/// Build the HTTP router. Caller owns binding + serving so the same router
/// can be reused in tests with a random port.
pub fn router<S: Storage>(storage: Arc<S>) -> Router {
    let state = AppState { storage };
    Router::new()
        .route("/health", get(health))
        .route("/memory/:id", get(get_memory).delete(delete_memory))
        .route("/memory", post(save_memory))
        .route("/memory/:id/touch", post(touch_memory))
        .route("/memory/:id/supersede", post(supersede_memory))
        .route("/search", post(search_memory))
        .route("/observe", post(observe_handler))
        // CORS open for any origin so the desktop renderer (file://) can hit
        // the localhost endpoint without preflight rejections. Bind to
        // 127.0.0.1 keeps the surface local-only.
        .layer(
            CorsLayer::new()
                .allow_methods(Any)
                .allow_headers(Any)
                .allow_origin(Any),
        )
        .with_state(state)
}

/// Bind and serve on `addr`. Blocks until cancelled.
pub async fn serve<S: Storage>(storage: Arc<S>, addr: &str) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %addr, "HTTP API listening");
    axum::serve(listener, router(storage)).await?;
    Ok(())
}

// `delete` route name was being shadowed by axum's `delete` import — keep
// the import explicit so the warning stays quiet on stable.
#[allow(unused_imports)]
use delete as _delete;
