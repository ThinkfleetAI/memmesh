// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Server crate — wires Storage / MCP / audit / sync together for the
//! long-running modes. Pure library; the `memmesh` binary lives
//! in `memory-cli` and calls into here.
//!
//! v1 surfaces:
//!   - `serve_mcp_stdio(storage)` — stdio-loop MCP server for AI tools
//!     (Claude Code, Cursor, Codex, Windsurf, etc.).
//!   - `serve_http(storage, addr)` — local-only REST API for the GUI
//!     (thinkfleet-desktop).
//!   - gRPC server lands in v1.1 alongside the sync engine.

mod http;

use anyhow::Result;
use memory_license::License;
use memory_storage::Storage;
use std::sync::Arc;

/// Run the MCP stdio server, dispatching tool calls to the given storage
/// backend. `license` gates every write-path tool so callers can't grow
/// memory past their plan's cap. Blocks until stdin closes.
pub async fn serve_mcp_stdio<S: Storage>(storage: Arc<S>, license: License) -> Result<()> {
    memory_mcp::run_stdio(storage, license).await
}

/// Run the HTTP REST API server (consumed by the desktop GUI). Binds to
/// `addr` (typically `127.0.0.1:7878`). Blocks until cancelled.
pub async fn serve_http<S: Storage>(storage: Arc<S>, addr: &str) -> Result<()> {
    http::serve(storage, addr).await
}
