// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Remote MCP server over **Streamable HTTP** — the transport ChatGPT and
//! Claude.ai custom connectors require (a local stdio server can't be
//! registered directly). This is deliberately separate from the loopback
//! console (`http.rs`, no auth): the remote MCP endpoint is meant to be
//! exposed publicly via a tunnel, so it **requires a bearer token** on every
//! request.
//!
//! Endpoints:
//!   POST /mcp     — one JSON-RPC message per request; returns the JSON-RPC
//!                   response as `application/json`, or `202` for
//!                   notifications. Requires `Authorization: Bearer <token>`.
//!   GET  /mcp     — 405 (we don't do server-initiated SSE streams).
//!   GET  /health  — unauthed liveness for tunnel/uptime checks.
//!
//! Stateless: no `Mcp-Session-Id` is issued — every request is handled
//! independently, which the MCP spec permits and our tools allow.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use memory_license::License;
use memory_storage::Storage;
use serde_json::json;
use std::sync::Arc;

struct McpState<S: Storage> {
    storage: Arc<S>,
    license: Arc<License>,
    token: Arc<String>,
}

impl<S: Storage> Clone for McpState<S> {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            license: self.license.clone(),
            token: self.token.clone(),
        }
    }
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.strip_prefix("Bearer ").unwrap_or(h).trim() == token)
        .unwrap_or(false)
}

async fn mcp_post<S: Storage>(
    State(st): State<McpState<S>>,
    headers: HeaderMap,
    body: String,
) -> axum::response::Response {
    if !authorized(&headers, &st.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized — send Authorization: Bearer <token>" })),
        )
            .into_response();
    }
    match memory_mcp::handle_message(&body, st.storage.as_ref(), &st.license).await {
        // Request → JSON-RPC response as application/json.
        Some(value) => (StatusCode::OK, Json(value)).into_response(),
        // Notification → no body, per JSON-RPC / MCP.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Serve the remote MCP endpoint on `addr`, gated by `token`. Blocks until
/// cancelled.
pub async fn serve_mcp_http<S: Storage>(
    storage: Arc<S>,
    license: License,
    addr: &str,
    token: String,
) -> anyhow::Result<()> {
    let state = McpState {
        storage,
        license: Arc::new(license),
        token: Arc::new(token),
    };
    let app = Router::new()
        .route(
            "/mcp",
            post(mcp_post::<S>).get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .route(
            "/health",
            get(|| async { Json(json!({ "name": "memmesh-mcp", "status": "ok" })) }),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %addr, "remote MCP (Streamable HTTP) server listening");
    axum::serve(listener, app).await?;
    Ok(())
}
