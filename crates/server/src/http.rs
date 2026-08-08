// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! HTTP REST API + web console for memmesh.
//!
//! Bound to loopback by default — this is a local-only management API,
//! not an internet-facing service. The `memmesh console` command serves
//! the embedded UI (see `ui/index.html`) from the same origin, so the
//! page talks to these endpoints with no CORS/auth friction.
//!
//! Endpoints:
//!
//!   GET    /                                — embedded web console (HTML)
//!   GET    /health                          — liveness + version + backend
//!   GET    /stats                           — counts by scope / status / type
//!   GET    /memory                          — list/search (query params)
//!   GET    /memory/:id                      — fetch one memory item
//!   POST   /memory                          — upsert
//!   DELETE /memory/:id                      — soft delete (status=rejected)
//!   DELETE /memory/:id?hard=true            — physical delete
//!   POST   /search                          — body = MemoryFilter+limit
//!   POST   /memory/:id/touch                — bump lastAccessedAt
//!   POST   /memory/:id/supersede            — body = { byId }
//!   POST   /consolidate                     — dedup (dryRun previews)
//!   GET    /config                          — read ~/.memmesh/config.toml
//!   PUT    /config                          — write config.toml (validated)
//!   GET    /logs?lines=N                    — tail the unified engine log
//!
//! Response shape is JSON; errors carry a stable `code` field for the UI
//! to switch on. No auth in v1 — loopback only. Token auth lands when the
//! API gets exposed beyond localhost.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use memory_core::{
    config::{Config, DatabaseBackend},
    MemoryItem, MemoryScope,
};
use memory_storage::{
    observe::{ObserveRequest, ObserveResponse},
    MemoryFilter, MemoryQuery, Storage, StorageError,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

/// The web console single-page app, baked into the binary at build time.
const INDEX_HTML: &str = include_str!("ui/index.html");

/// Parse a scope string to `MemoryScope`, returning `None` for unknown
/// values (treated as "no scope filter" by the query layer).
fn parse_scope_opt(s: Option<String>) -> Option<MemoryScope> {
    match s.as_deref()? {
        "platform" => Some(MemoryScope::Platform),
        "project" => Some(MemoryScope::Project),
        "location" => Some(MemoryScope::Location),
        "agent" => Some(MemoryScope::Agent),
        "user" => Some(MemoryScope::User),
        "session" => Some(MemoryScope::Session),
        _ => None,
    }
}

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

// ── Web console (embedded SPA) ──────────────────────────────

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

// ── /stats ──────────────────────────────────────────────────

async fn stats<S: Storage>(State(s): State<AppState<S>>) -> axum::response::Response {
    match s.storage.memory_stats(&MemoryFilter::default()).await {
        Ok(st) => Json(json!({
            "total": st.total,
            "byScope": st.by_scope,
            "byStatus": st.by_status,
            "byType": st.by_type,
            "withEmbedding": st.with_embedding,
            "withoutEmbedding": st.without_embedding,
            "patternCount": st.pattern_count,
        }))
        .into_response(),
        Err(e) => map_err(e),
    }
}

// ── GET /memory (list / text search) ────────────────────────

#[derive(Deserialize)]
struct ListQuery {
    query: Option<String>,
    project: Option<String>,
    scope: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

async fn list_memory<S: Storage>(
    State(s): State<AppState<S>>,
    Query(q): Query<ListQuery>,
) -> axum::response::Response {
    let filter = MemoryFilter {
        project_id: q.project,
        scope: parse_scope_opt(q.scope),
        kind: q.kind,
        ..Default::default()
    };
    // Hybrid semantic + lexical + recency ranking; empty query returns the
    // newest items ranked by recency (falls back to lexical when embeddings
    // are off). Same path the CLI `search` uses.
    match memory_storage::search::search(
        s.storage.as_ref(),
        &filter,
        q.query.as_deref(),
        q.limit.unwrap_or(50),
        q.offset.unwrap_or(0),
    )
    .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err("search", StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── /consolidate ────────────────────────────────────────────

#[derive(Deserialize)]
struct ConsolidateBody {
    project: Option<String>,
    scope: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    threshold: Option<f32>,
    // The web console sends camelCase `dryRun`; keep `dry_run` as an alias so
    // scripts/CLI-style callers work too. Getting this wrong is dangerous: a
    // missed `dryRun` silently turns a "preview" into a real collapse.
    #[serde(default, rename = "dryRun", alias = "dry_run")]
    dry_run: bool,
}

async fn consolidate_handler<S: Storage>(
    State(s): State<AppState<S>>,
    Json(body): Json<ConsolidateBody>,
) -> axum::response::Response {
    let filter = MemoryFilter {
        project_id: body.project,
        scope: parse_scope_opt(body.scope),
        kind: body.kind,
        ..Default::default()
    };
    let threshold = body.threshold.unwrap_or(0.95);
    match memory_storage::consolidate::consolidate(s.storage.as_ref(), &filter, threshold, body.dry_run)
        .await
    {
        Ok(r) => Json(json!({
            "scanned": r.scanned,
            "threshold": r.threshold,
            "semantic": r.semantic,
            "dryRun": r.dry_run,
            "collapsed": r.collapses.len(),
            "collapses": r.collapses.iter().map(|c| json!({
                "loserId": c.loser_id,
                "survivorId": c.survivor_id,
                "similarity": c.similarity,
            })).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => err("consolidate", StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── /config ─────────────────────────────────────────────────

async fn get_config() -> axum::response::Response {
    let path = Config::resolve_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    // Empty/absent file → hand back the serialized effective defaults so the
    // editor shows a real starting point instead of a blank box.
    let toml_text = if text.trim().is_empty() {
        toml::to_string_pretty(&Config::load_or_default()).unwrap_or_default()
    } else {
        text
    };
    Json(json!({ "path": path.display().to_string(), "toml": toml_text })).into_response()
}

async fn put_config(body: String) -> axum::response::Response {
    // Validate before persisting so a typo can't leave the engine unable to
    // start. We write the user's raw text (comments preserved), not a
    // re-serialized struct.
    if let Err(e) = toml::from_str::<Config>(&body) {
        return err("config_invalid", StatusCode::BAD_REQUEST, format!("invalid config TOML: {e}"));
    }
    let path = Config::resolve_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, &body) {
        Ok(()) => Json(json!({ "ok": true, "path": path.display().to_string() })).into_response(),
        Err(e) => err("config_write", StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── /logs ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct LogsQuery {
    lines: Option<usize>,
}

async fn logs_handler(Query(q): Query<LogsQuery>) -> axum::response::Response {
    let path = Config::log_file();
    let n = q.lines.unwrap_or(200).min(5000);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(n);
    let tail: Vec<String> = all[start..].iter().map(|s| s.to_string()).collect();
    Json(json!({
        "path": path.display().to_string(),
        "total": all.len(),
        "lines": tail,
    }))
    .into_response()
}

// ── /database (backend selection + migration) ───────────────

/// Report the storage backend this process is actually running on
/// (`current`) versus what config would use on next start (`configured`).
/// They differ after a switch until the engine is restarted.
async fn get_database<S: Storage>(State(s): State<AppState<S>>) -> axum::response::Response {
    let cfg = Config::load_or_default();
    Json(json!({
        "current": s.storage.backend(),
        "configured": cfg.database.backend.as_str(),
        "url": cfg.database.url,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct DbUrlBody {
    url: String,
}

/// Probe a Postgres URL without saving anything — validates host, creds, and
/// reachability so the user gets a clear yes/no before committing. Bounded by
/// a short timeout: sqlx's default acquire timeout is ~30s, which would make a
/// wrong host or port feel like a hang in the UI.
async fn test_database(Json(b): Json<DbUrlBody>) -> axum::response::Response {
    let connect = memory_storage::postgres::PostgresStore::connect(&b.url);
    let body = match tokio::time::timeout(std::time::Duration::from_secs(6), connect).await {
        Ok(Ok(_)) => json!({ "ok": true, "message": "connected" }),
        Ok(Err(e)) => json!({ "ok": false, "message": e.to_string() }),
        Err(_) => json!({ "ok": false, "message": "connection timed out after 6s — check host/port" }),
    };
    Json(body).into_response()
}

/// One-time copy of memory items from the CURRENT backend into a Postgres
/// target: connect, run migrations, then page through and re-`save` each row.
/// Idempotent (save is an upsert), so re-running is safe.
async fn copy_database<S: Storage>(
    State(s): State<AppState<S>>,
    Json(b): Json<DbUrlBody>,
) -> axum::response::Response {
    let dest = match memory_storage::postgres::PostgresStore::connect(&b.url).await {
        Ok(d) => d,
        Err(e) => return err("pg_connect", StatusCode::BAD_GATEWAY, e.to_string()),
    };
    if let Err(e) = dest.migrate().await {
        return err("pg_migrate", StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let page: u32 = 500;
    let mut offset: u32 = 0;
    let mut copied: u64 = 0;
    loop {
        let rows = match s
            .storage
            .query(&MemoryQuery {
                filter: MemoryFilter::default(),
                limit: Some(page),
                offset: Some(offset),
            })
            .await
        {
            Ok(r) => r,
            Err(e) => return map_err(e),
        };
        if rows.is_empty() {
            break;
        }
        let n = rows.len() as u32;
        for item in &rows {
            if let Err(e) = dest.save(item).await {
                return err(
                    "pg_write",
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("copied {copied} item(s) then failed: {e}"),
                );
            }
            copied += 1;
        }
        if n < page {
            break;
        }
        offset += page;
    }
    let dest_total = dest.count_items().await.unwrap_or(-1);
    Json(json!({ "copied": copied, "destTotal": dest_total })).into_response()
}

#[derive(Deserialize)]
struct SetDbBody {
    backend: String,
    #[serde(default)]
    url: Option<String>,
}

/// Persist the backend choice to `~/.memmesh/config.toml`. Takes effect on
/// the next start of each `memmesh` process.
async fn put_database(Json(b): Json<SetDbBody>) -> axum::response::Response {
    let backend = match b.backend.as_str() {
        "sqlite" => DatabaseBackend::Sqlite,
        "postgres" => DatabaseBackend::Postgres,
        other => {
            return err(
                "bad_backend",
                StatusCode::BAD_REQUEST,
                format!("unknown backend '{other}'"),
            )
        }
    };
    let url = b.url.filter(|u| !u.trim().is_empty());
    if backend == DatabaseBackend::Postgres && url.is_none() {
        return err(
            "missing_url",
            StatusCode::BAD_REQUEST,
            "postgres backend requires a connection url",
        );
    }
    let mut cfg = Config::load_or_default();
    cfg.database.backend = backend;
    cfg.database.url = url;
    match cfg.save() {
        Ok(()) => Json(json!({ "ok": true, "backend": backend.as_str() })).into_response(),
        Err(e) => err("config_write", StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── Public entrypoint ───────────────────────────────────────

/// Build the HTTP router. Caller owns binding + serving so the same router
/// can be reused in tests with a random port.
pub fn router<S: Storage>(storage: Arc<S>) -> Router {
    let state = AppState { storage };
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/stats", get(stats))
        .route("/memory/:id", get(get_memory).delete(delete_memory))
        .route("/memory", post(save_memory).get(list_memory))
        .route("/memory/:id/touch", post(touch_memory))
        .route("/memory/:id/supersede", post(supersede_memory))
        .route("/search", post(search_memory))
        .route("/observe", post(observe_handler))
        .route("/consolidate", post(consolidate_handler))
        .route("/config", get(get_config).put(put_config))
        .route("/database", get(get_database).put(put_database))
        .route("/database/test", post(test_database))
        .route("/database/copy", post(copy_database))
        .route("/logs", get(logs_handler))
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
