// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Model Context Protocol (MCP) stdio server.
//!
//! Exposes the engine as a universal MCP server so any MCP-capable AI tool
//! (Claude Code, Cursor, Codex, Windsurf, ChatGPT desktop, custom agents)
//! can plug in with a single config block and get persistent hierarchical
//! memory.
//!
//! Wire format: line-delimited JSON-RPC 2.0 over stdin/stdout. Each request
//! is a single line of JSON; each response is a single line of JSON.
//!
//! Tools surfaced (underscore names — dots aren't universally legal in MCP
//! tool names across clients):
//!   - `memory_observe`  feed raw text; the engine decides what to save
//!   - `memory_save`     upsert a memory item explicitly (rare)
//!   - `memory_recall`   fetch a memory item by id
//!   - `memory_search`   text / scope search with ranking
//!   - `memory_list`     enumerate items in a scope
//!
//! Anything that needs vector recall, pattern detection, or sync goes
//! through follow-up tool registrations in v1.1.

use anyhow::Result;
use chrono::Utc;
use memory_core::{MemoryEdge, MemoryItem, MemoryScope};
use memory_license::License;
use memory_storage::{
    entity_resolver::{resolve_or_create_entity, EntityContext},
    observe::ObserveRequest,
    MemoryFilter, MemoryQuery, Storage,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "memmesh";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

impl JsonRpcResponse {
    fn ok(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn err(id: Option<serde_json::Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

/// Run the MCP stdio loop until stdin closes. `license` gates every
/// write-path tool (memory_save / memory_observe / memory_commit_extraction)
/// so callers can't grow memory past their plan's cap.
pub async fn run_stdio<S: Storage>(storage: Arc<S>, license: License) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    tracing::info!(license = %license.describe(), "MCP stdio server started ({SERVER_NAME} {SERVER_VERSION})");

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        // `handle_line` returns `None` for notifications (no id) — the
        // JSON-RPC 2.0 spec is explicit: servers MUST NOT respond to
        // notifications. The Claude Code MCP client enforces this with a
        // strict Zod schema; sending `id: null` responses gets the
        // connection rejected.
        if let Some(response) = handle_line(&line, storage.as_ref(), &license).await {
            let serialized = serde_json::to_string(&response).unwrap_or_else(|e| {
                format!(
                    r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"{e}"}}}}"#
                )
            });
            stdout.write_all(serialized.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    tracing::info!("MCP stdio server exiting (stdin closed)");
    Ok(())
}

/// Handle a single JSON-RPC message (raw string) and return the serialized
/// response value, or `None` for notifications (which get no response). Shared
/// by the stdio loop and the Streamable-HTTP transport in `memory-server`.
pub async fn handle_message<S: Storage>(
    message: &str,
    storage: &S,
    license: &License,
) -> Option<serde_json::Value> {
    handle_line(message, storage, license).await.map(|resp| {
        serde_json::to_value(resp).unwrap_or_else(|e| {
            json!({ "jsonrpc": "2.0", "id": null,
                    "error": { "code": -32603, "message": e.to_string() } })
        })
    })
}

// Both early-return blocks below check id-presence and need readable control
// flow with side effects (tracing). The clippy `?` rewrite hides intent.
#[allow(clippy::question_mark)]
async fn handle_line<S: Storage>(
    line: &str,
    storage: &S,
    license: &License,
) -> Option<JsonRpcResponse> {
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return Some(JsonRpcResponse::err(
                None,
                -32700,
                format!("parse error: {e}"),
            ))
        }
    };
    if req.jsonrpc != "2.0" {
        // If this came in as a notification, suppress the error — JSON-RPC
        // says no responses to notifications, even on parse failure of
        // method/params.
        if req.id.is_none() {
            return None;
        }
        return Some(JsonRpcResponse::err(
            req.id,
            -32600,
            "jsonrpc must be \"2.0\"",
        ));
    }

    // Notifications (id == null and method present): MUST NOT respond.
    // These include MCP's `notifications/initialized`,
    // `notifications/cancelled`, `notifications/progress`, etc. We
    // process them for side effects only — currently logging.
    if req.id.is_none() {
        tracing::debug!(method = %req.method, "notification (no response)");
        return None;
    }

    let response = match req.method.as_str() {
        "initialize" => {
            // Echo the client's requested protocol version when present. stdio
            // clients (Claude Code) send 2024-11-05; Streamable-HTTP clients
            // (ChatGPT / Claude.ai connectors) negotiate newer revisions
            // (2025-03-26 / 2025-06-18). Our tool surface is compatible across
            // all of them, so echoing avoids a version-mismatch rejection.
            let negotiated = req
                .params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_VERSION)
                .to_string();
            JsonRpcResponse::ok(
                req.id,
                json!({
                    "protocolVersion": negotiated,
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "version": SERVER_VERSION,
                    }
                }),
            )
        }

        "tools/list" => JsonRpcResponse::ok(req.id, json!({ "tools": tool_definitions() })),

        "tools/call" => match handle_tool_call(&req.params, storage, license).await {
            Ok(value) => JsonRpcResponse::ok(req.id, value),
            Err(e) => JsonRpcResponse::err(req.id, -32603, e.to_string()),
        },

        "ping" => JsonRpcResponse::ok(req.id, json!({})),

        method => JsonRpcResponse::err(req.id, -32601, format!("unknown method: {method}")),
    };

    Some(response)
}

fn tool_definitions() -> serde_json::Value {
    json!([
        {
            "name": "memory_observe",
            "description": "**Call this AFTER EVERY USER MESSAGE.** Send the user's raw text to the memory engine — it runs heuristic extraction (regex + structural rules) and decides what's worth saving. You do NOT need to filter or judge what to save; that's the engine's job. PREFER THIS over memory_save for any routine observation. Returns the list of items saved (may be empty for filler — that's fine, ignore the response). Cheap, idempotent (re-observing the same text is a no-op), zero LLM cost in the default path.",
            "inputSchema": {
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text":       { "type": "string", "description": "Raw text to observe — the user's exact message, an assistant turn, a transcript fragment." },
                    "role":       { "type": "string", "enum": ["user", "assistant", "system"], "description": "Who said the text. Defaults to 'user'." },
                    "platformId": { "type": ["string", "null"], "description": "Defaults to 'local'." },
                    "projectId":  { "type": ["string", "null"], "description": "Current project (git repo name is a good default)." },
                    "userId":     { "type": ["string", "null"], "description": "Defaults to the OS username." },
                    "agentId":    { "type": ["string", "null"] },
                    "sessionId":  { "type": ["string", "null"] },
                    "occurredAt": { "type": ["string", "null"], "description": "RFC3339 timestamp of when this happened IN THE WORLD, not when you're recording it. Defaults to now. Set it when observing anything back-dated — behavior mining buckets patterns by this timestamp." }
                }
            }
        },
        {
            "name": "memory_save",
            "description": "Save a memory item explicitly with a specific id, type, scope. **Rarely needed — prefer memory_observe for routine messages.** Use this only when the user explicitly says 'please save the following note verbatim' or when you have structured data to record that won't survive the engine's extraction heuristics.",
            "inputSchema": {
                "type": "object",
                "required": ["id", "platformId", "type", "content", "scope"],
                "properties": {
                    "id":           { "type": "string", "description": "Stable unique id (21+ chars)." },
                    "platformId":   { "type": "string" },
                    "projectId":    { "type": ["string", "null"] },
                    "agentId":      { "type": ["string", "null"], "description": "Maps to chatbotId in the underlying schema." },
                    "userId":       { "type": ["string", "null"], "description": "Maps to chatIdentityId." },
                    "sessionId":    { "type": ["string", "null"], "description": "Maps to sessionKey." },
                    "type":         { "type": "string", "description": "fact / preference / rule / etc." },
                    "content":      { "type": "string" },
                    "scope":        { "type": "string", "enum": ["platform","project","location","agent","user","session"] },
                    "importance":   { "type": "number", "description": "0-10, default 5." },
                    "confidence":   { "type": "number", "description": "0-1, default 1.0." },
                    "occurredAt":   { "type": ["string", "null"], "description": "RFC3339 timestamp of when this happened IN THE WORLD, as opposed to when you're recording it. Defaults to now. Set this whenever you're recording something back-dated (importing history, logging a past event) — behavior mining buckets patterns by this timestamp, so leaving it unset makes every backfilled event look like it happened at import time." },
                    "metadata":     { "type": ["object", "null"] }
                }
            }
        },
        {
            "name": "memory_recall",
            "description": "Fetch a single memory item by its id. Bumps lastAccessedAt as a side effect (drives recency scoring). Use after memory_search returns ids you want full detail on.",
            "inputSchema": {
                "type": "object",
                "required": ["id"],
                "properties": { "id": { "type": "string" } }
            }
        },
        {
            "name": "memory_search",
            "description": "**Call this AT THE START OF EVERY SESSION** to load relevant persistent context before responding. Filters by scope / project / agent / user / session / type with optional substring match. Returns up to `limit` rows ordered newest-first. If you don't know the project, pass userId only. Skip only on pure pleasantries; the moment the user says anything substantive, search first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "platformId":   { "type": ["string", "null"] },
                    "projectId":    { "type": ["string", "null"] },
                    "agentId":      { "type": ["string", "null"] },
                    "userId":       { "type": ["string", "null"] },
                    "sessionId":    { "type": ["string", "null"] },
                    "scope":        { "type": ["string", "null"], "enum": ["platform","project","location","agent","user","session", null] },
                    "type":         { "type": ["string", "null"] },
                    "query":        { "type": ["string", "null"], "description": "Substring to match against content." },
                    "limit":        { "type": "integer", "default": 20 },
                    "offset":       { "type": "integer", "default": 0 }
                }
            }
        },
        {
            "name": "memory_list",
            "description": "Alias of memory_search with no query — returns the most recent memories in a scope. Useful for an agent to skim what's in memory without a specific query.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "platformId": { "type": ["string", "null"] },
                    "projectId":  { "type": ["string", "null"] },
                    "scope":      { "type": ["string", "null"] },
                    "limit":      { "type": "integer", "default": 20 },
                    "offset":     { "type": "integer", "default": 0, "description": "Skip this many rows — page by bumping it. The handler has always honored this; it was just missing from the schema, so agents had no way to know they could page." }
                }
            }
        },
        {
            "name": "memory_extract_pending",
            "description": "Return memories whose knowledge-graph entities have not been extracted yet, each accompanied by an LLM-ready extraction prompt. The MCP CLIENT (the agent) runs each prompt through its own LLM, parses the JSON response, then calls `memory_commit_extraction` to persist the result. The memory engine never makes LLM calls itself — your model + your API key + your rate limit. Idempotency: a memory whose `metadata.extractionContentHash` already matches the current md5(content) is skipped automatically. Typical loop: agent calls extract_pending → processes each prompt locally → calls commit_extraction → repeat until response is empty.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "platformId": { "type": ["string", "null"], "description": "Defaults to 'local'." },
                    "projectId":  { "type": ["string", "null"], "description": "Restrict to one project. Defaults to no project filter." },
                    "limit":      { "type": "integer", "default": 10, "description": "Max memories to return (1-50). Cap chosen so the agent can process them serially without timing out." }
                }
            }
        },
        {
            "name": "memory_commit_extraction",
            "description": "Persist the entities + edges your LLM extracted from a memory, returned in response to `memory_extract_pending`. Engine resolves each entity by canonical name (dedupes across passes), writes typed edges with `sourceMemoryId` pointing back to the source memory, then stamps `metadata.extractionContentHash` + `metadata.lastExtractionAt` on the source memory so the same content isn't re-extracted on the next call.\n\nEntity types allowed: person, org, product, location, concept, event, document, other. Predicates should be snake_case verbs (e.g. purchased, prefers, works_at, located_in, has_condition). Use `object_is_entity: true` only when the object also appears in `entities`; otherwise the object is treated as a literal value.",
            "inputSchema": {
                "type": "object",
                "required": ["memoryId", "contentHash", "entities", "edges"],
                "properties": {
                    "memoryId":    { "type": "string", "description": "id from extract_pending response." },
                    "contentHash": { "type": "string", "description": "The md5 hash from extract_pending. If the memory content has changed since extract_pending returned it, the commit is rejected so the agent can re-fetch." },
                    "entities": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["name", "type"],
                            "properties": {
                                "name": { "type": "string", "description": "Verbatim text from the memory; will be canonicalized by entity_resolver." },
                                "type": { "type": "string", "enum": ["person","org","product","location","concept","event","document","other"] }
                            }
                        }
                    },
                    "edges": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["subject", "predicate", "object"],
                            "properties": {
                                "subject":         { "type": "string", "description": "Must match an entity.name from the entities array." },
                                "predicate":       { "type": "string", "description": "snake_case verb." },
                                "object":          { "type": "string", "description": "Either an entity.name (if object_is_entity=true) or a literal value." },
                                "object_is_entity": { "type": "boolean", "default": false }
                            }
                        }
                    }
                }
            }
        },
        {
            "name": "memory_delete",
            "description": "Delete a memory by id. Soft-delete by default (marks it deleted, recoverable); pass hard=true to remove the row permanently. Use to correct a mistake or honor a 'forget this' request.",
            "inputSchema": {
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id":   { "type": "string", "description": "id of the memory to delete." },
                    "hard": { "type": "boolean", "default": false, "description": "true = permanent hard delete; false = recoverable soft delete." }
                }
            }
        },
        {
            "name": "memory_supersede",
            "description": "Mark one memory as superseded by another — the old memory is kept for audit/history but is no longer the current truth. Use when a fact changes ('actually we moved to Postgres'): save the new memory, then supersede the old one by the new id.",
            "inputSchema": {
                "type": "object",
                "required": ["id", "byId"],
                "properties": {
                    "id":   { "type": "string", "description": "id of the memory being superseded (the old / outdated one)." },
                    "byId": { "type": "string", "description": "id of the memory that replaces it (the new current one)." }
                }
            }
        },
        {
            "name": "memory_stats",
            "description": "Return counts about the memory store — currently the total number of memories held. Useful for a health check or a 'how much do you remember' summary.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "memory_consolidate",
            "description": "Find near-duplicate memories in a scope and non-destructively collapse the redundant ones into a survivor (via supersede — history is kept). Two memories are duplicates when their embeddings' cosine similarity is >= threshold. Safe + idempotent: re-running skips already-collapsed items. Use dryRun=true first to preview. Requires semantic embeddings; falls back to exact normalized-text equality when embeddings are off.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "platformId": { "type": ["string", "null"] },
                    "projectId":  { "type": ["string", "null"] },
                    "agentId":    { "type": ["string", "null"] },
                    "userId":     { "type": ["string", "null"] },
                    "scope":      { "type": ["string", "null"], "enum": ["platform","project","location","agent","user","session", null] },
                    "type":       { "type": ["string", "null"] },
                    "threshold":  { "type": "number", "default": 0.95, "description": "Cosine similarity at/above which two memories are collapsed. Higher = stricter." },
                    "dryRun":     { "type": "boolean", "default": false, "description": "Preview collapses without writing." }
                }
            }
        },
        {
            "name": "memory_secret_list",
            "description": "List the names/kinds/descriptions of credentials in the encrypted vault. NEVER returns values — you cannot read a secret, only reference it. Use before secret_run to see what's available.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "memory_secret_request",
            "description": "Check whether a credential is available and, if not, get instructions to have the USER add it. Call this when you need a credential (API key, password, token). It returns status only — never a value. If missing, tell the user to add it via `memmesh secret set <name>` or the console Vault tab; NEVER ask the user to paste a secret into the chat.",
            "inputSchema": {
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name":    { "type": "string", "description": "Reference name, e.g. 'aws-prod'." },
                    "purpose": { "type": "string", "description": "Why you need it (shown to the user)." }
                }
            }
        },
        {
            "name": "memory_secret_run",
            "description": "Execute-through-vault: run a shell command that references vault secrets as {{memmesh:NAME}} placeholders. The engine substitutes the real values IN ITS OWN PROCESS, runs the command, and returns stdout/stderr with every secret value scrubbed to [redacted]. This is the ONLY way to use a secret — you never receive the plaintext. Example command: \"aws s3 ls --profile {{memmesh:aws-prod}}\".",
            "inputSchema": {
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": { "type": "string", "description": "Shell command containing {{memmesh:NAME}} references." }
                }
            }
        }
    ])
}

async fn handle_tool_call<S: Storage>(
    params: &serde_json::Value,
    storage: &S,
    license: &License,
) -> anyhow::Result<serde_json::Value> {
    #[derive(Deserialize)]
    struct CallParams {
        name: String,
        #[serde(default)]
        arguments: serde_json::Value,
    }
    let call: CallParams = serde_json::from_value(params.clone())?;
    let args = &call.arguments;

    // Accept both underscore (canonical) and dot (legacy) names. The server
    // historically defined tools with dots; most MCP clients silently rewrite
    // dots to underscores when exposing them to the agent, so different
    // clients have been sending different shapes. Both work; the canonical
    // form going forward is underscore.
    let canonical = call.name.replace('.', "_");
    match canonical.as_str() {
        "memory_observe" => {
            let req: ObserveRequest = serde_json::from_value(args.clone())?;
            // Cap-gate before extraction. The engine can extract many items
            // from a single observe() call; if we're already at the cap, fail
            // fast instead of running the extractor for nothing.
            if let Err(e) =
                memory_storage::quota::ensure_under_cap(storage, license.cap()).await
            {
                return Ok(text_result(&format!("rejected: {e}")));
            }
            let resp = memory_storage::observe::observe(storage, &req).await?;
            let summary = if resp.saved.is_empty() {
                format!(
                    "observed; nothing memorable extracted ({} candidate(s))",
                    resp.candidate_count
                )
            } else {
                let lines: Vec<String> = resp
                    .saved
                    .iter()
                    .map(|m| format!("  - [{}] {} ({})", m.scope.as_str(), m.content, m.type_))
                    .collect();
                format!(
                    "saved {} memor{}:\n{}",
                    resp.saved.len(),
                    if resp.saved.len() == 1 { "y" } else { "ies" },
                    lines.join("\n"),
                )
            };
            Ok(text_result(&summary))
        }

        "memory_save" => {
            let item = memory_item_from_args(args)?;
            // Cap-gate on the license. Free tier = 500-item cap. Paid plans
            // carry their own cap claim (u64::MAX for unlimited).
            if let Err(e) =
                memory_storage::quota::ensure_under_cap(storage, license.cap()).await
            {
                return Ok(text_result(&format!("rejected: {e}")));
            }
            storage.save(&item).await?;
            // Index for semantic search (no-op when embeddings are off).
            memory_storage::embedding::embed_and_store(storage, &item.id, &item.content).await;
            Ok(text_result(&format!("saved {}", item.id)))
        }

        "memory_recall" => {
            let id = arg_str(args, "id")?;
            let found = storage.get(id).await?;
            if let Some(item) = &found {
                storage.touch(&item.id).await.ok(); // bump recency, ignore error
            }
            Ok(text_result(&serde_json::to_string_pretty(&found)?))
        }

        "memory_search" | "memory_list" => {
            let mut filter = filter_from_args(args)?;
            // The free-text query is carried in `text_match` by filter_from_args;
            // pull it out and route it through the hybrid searcher (semantic +
            // lexical + recency). `memory_list` (no query) degrades to recency
            // order automatically.
            let query = filter.text_match.take();
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let rows = memory_storage::search::search(
                storage,
                &filter,
                query.as_deref(),
                limit,
                offset,
            )
            .await?;
            Ok(text_result(&serde_json::to_string_pretty(&rows)?))
        }

        "memory_extract_pending" => extract_pending(storage, args).await,

        "memory_commit_extraction" => commit_extraction(storage, args).await,

        "memory_delete" => {
            let id = arg_str(args, "id")?;
            let hard = args.get("hard").and_then(|v| v.as_bool()).unwrap_or(false);
            storage.delete(id, hard).await?;
            Ok(text_result(&format!(
                "{} memory {id}",
                if hard { "hard-deleted" } else { "deleted" }
            )))
        }

        "memory_supersede" => {
            let id = arg_str(args, "id")?;
            let by_id = arg_str(args, "byId")?;
            storage.supersede(id, by_id).await?;
            Ok(text_result(&format!("superseded {id} by {by_id}")))
        }

        "memory_stats" => {
            let total = storage.count_items().await?;
            Ok(text_result(&serde_json::to_string_pretty(
                &serde_json::json!({ "totalMemories": total }),
            )?))
        }

        "memory_consolidate" => {
            let filter = MemoryFilter {
                platform_id: arg_opt(args, "platformId"),
                project_id: arg_opt(args, "projectId"),
                agent_id: arg_opt(args, "agentId"),
                user_id: arg_opt(args, "userId"),
                scope: args
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .map(scope_from_str)
                    .transpose()?,
                kind: arg_opt(args, "type"),
                ..Default::default()
            };
            let threshold = args.get("threshold").and_then(|v| v.as_f64()).unwrap_or(0.95) as f32;
            let dry_run = args.get("dryRun").and_then(|v| v.as_bool()).unwrap_or(false);
            let report =
                memory_storage::consolidate::consolidate(storage, &filter, threshold, dry_run)
                    .await?;
            let lines: Vec<String> = report
                .collapses
                .iter()
                .map(|c| format!("  {} → {} (cosine {:.3})", c.loser_id, c.survivor_id, c.similarity))
                .collect();
            let matcher = if report.semantic { "cosine embeddings" } else { "exact text (no embeddings)" };
            let mode = if report.dry_run { " (dry run — nothing written)" } else { "" };
            let summary = format!(
                "consolidate: scanned {} item(s), collapsed {} duplicate(s) at threshold {:.2} via {}{}{}",
                report.scanned,
                report.collapses.len(),
                report.threshold,
                matcher,
                mode,
                if lines.is_empty() { String::new() } else { format!("\n{}", lines.join("\n")) },
            );
            Ok(text_result(&summary))
        }

        // ── Secrets vault (the AI references, never reads) ──────────
        "memory_secret_list" => {
            let vault = open_vault().await?;
            let secrets = vault.list().await?;
            Ok(text_result(&serde_json::to_string_pretty(&secrets)?))
        }

        "memory_secret_request" => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            if name.is_empty() {
                return Ok(text_result("error: 'name' is required"));
            }
            let purpose = args.get("purpose").and_then(|v| v.as_str()).unwrap_or("(unspecified)");
            let vault = open_vault().await?;
            if vault.exists(name).await? {
                Ok(text_result(&format!(
                    "Secret '{name}' is available. Use it by calling memory_secret_run with \
                     {{{{memmesh:{name}}}}} in the command — you will never see its value."
                )))
            } else {
                Ok(text_result(&format!(
                    "Secret '{name}' is NOT set (purpose: {purpose}). Prompt the user to enter it \
                     securely — DO NOT ask them to paste the value into the chat. Give them this \
                     one-click link, which opens the memmesh console's Vault tab with the name \
                     pre-filled:\n\n  http://127.0.0.1:7878/?tab=vault&add={name}\n\n\
                     Or they can run `memmesh secret set {name}`. Once they've added it, retry \
                     using {{{{memmesh:{name}}}}} via memory_secret_run."
                )))
            }
        }

        "memory_secret_run" => {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or_default();
            if command.is_empty() {
                return Ok(text_result("error: 'command' is required"));
            }
            let vault = open_vault().await?;
            match vault.run(command).await {
                Ok(r) => Ok(text_result(&serde_json::to_string_pretty(&serde_json::json!({
                    "exitCode": r.exit_code,
                    "stdout": r.stdout,
                    "stderr": r.stderr,
                    "usedSecrets": r.used,
                    "note": "secret values are scrubbed from this output",
                }))?)),
                Err(e) => Ok(text_result(&format!("secret_run failed: {e}"))),
            }
        }

        other => anyhow::bail!("unknown tool: {other}"),
    }
}

/// Open the encrypted secrets vault (its own store + keychain-backed key).
async fn open_vault() -> anyhow::Result<memory_storage::vault::Vault> {
    memory_storage::vault::Vault::open(&memory_storage::vault::Vault::default_path()).await
}

// ─── Extraction tools (PR F) ─────────────────────────────────────────
//
// Two-step protocol: extract_pending hands raw text + a deterministic
// prompt to the agent; the agent's own LLM does the parse; commit
// persists the result. Engine never touches an LLM API.

const EXTRACTION_PROMPT_SYSTEM: &str = "You are an entity-relationship extractor. From the user's text, identify named entities (people, organizations, products, locations, concepts, events, documents) and the typed relationships between them. Return STRICT JSON only — no markdown fences, no explanations. Skip rather than guess: false positives are worse than misses.";

const EXTRACTION_PROMPT_USER_TEMPLATE: &str = r#"Extract entities and relationships. Return JSON in exactly this shape:

{
  "entities": [
    { "name": "Sarah", "type": "person" },
    { "name": "Acme Pizza", "type": "org" }
  ],
  "edges": [
    { "subject": "Sarah", "predicate": "purchased", "object": "Acme Pizza", "object_is_entity": true },
    { "subject": "Sarah", "predicate": "has_dietary_restriction", "object": "celiac", "object_is_entity": false }
  ]
}

Entity types: person, org, product, location, concept, event, document, other.
- "name" must appear verbatim in the text.
- Use "object_is_entity": true only when the object is itself a named entity that also appears in "entities".
- Use "object_is_entity": false for literals (zip codes, dates, dietary labels, free-text categories).
- Skip the entity entirely if you can't pick a type with confidence.

If the text has no entities, return {"entities":[],"edges":[]}.

Then call memory_commit_extraction with:
  memoryId:    "{{memoryId}}"
  contentHash: "{{contentHash}}"
  entities:    <your entities array>
  edges:       <your edges array>

TEXT:
{{content}}"#;

async fn extract_pending<S: Storage>(
    storage: &S,
    args: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let platform_id = arg_opt(args, "platformId").unwrap_or_else(|| "local".to_string());
    let project_id = arg_opt(args, "projectId");
    let limit_in = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10).clamp(1, 50) as u32;

    // Overscan by 4x because we filter out already-extracted rows in
    // Rust (the trait has no JSON-path predicate). For a project with
    // mostly-extracted memories this still finds work; the cap on the
    // raw query bounds memory pressure.
    let scan_limit = (limit_in * 4).min(500);
    let filter = MemoryFilter {
        platform_id: Some(platform_id),
        project_id,
        ..Default::default()
    };
    let rows = storage
        .query(&MemoryQuery { filter, limit: Some(scan_limit), offset: Some(0) })
        .await?;

    let mut pending = Vec::with_capacity(limit_in as usize);
    for item in rows.into_iter() {
        if item.content.trim().len() < 12 {
            continue;
        }
        let current_hash = md5_hex(&item.content);
        let already_hash = item
            .metadata
            .get("extractionContentHash")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if already_hash.as_deref() == Some(&current_hash) {
            continue;
        }
        let prompt = EXTRACTION_PROMPT_USER_TEMPLATE
            .replace("{{memoryId}}", &item.id)
            .replace("{{contentHash}}", &current_hash)
            .replace("{{content}}", &item.content);
        pending.push(json!({
            "memoryId": item.id,
            "contentHash": current_hash,
            "type": item.type_,
            "scope": item.scope.as_str(),
            "content": item.content,
            "systemPrompt": EXTRACTION_PROMPT_SYSTEM,
            "userPrompt": prompt,
        }));
        if pending.len() >= limit_in as usize {
            break;
        }
    }

    let summary = format!(
        "{} memor{} pending extraction (returned {}, scan limit {}). Process each `userPrompt` through your LLM, then call memory_commit_extraction for each.",
        pending.len(),
        if pending.len() == 1 { "y" } else { "ies" },
        pending.len(),
        scan_limit,
    );

    Ok(json!({
        "content": [
            { "type": "text", "text": summary },
            { "type": "text", "text": serde_json::to_string_pretty(&pending)? }
        ]
    }))
}

#[derive(Deserialize)]
struct CommitArgs {
    #[serde(rename = "memoryId")]
    memory_id: String,
    #[serde(rename = "contentHash")]
    content_hash: String,
    #[serde(default)]
    entities: Vec<CommitEntity>,
    #[serde(default)]
    edges: Vec<CommitEdge>,
}

#[derive(Deserialize)]
struct CommitEntity {
    name: String,
    #[serde(rename = "type", default)]
    type_: String,
}

#[derive(Deserialize)]
struct CommitEdge {
    subject: String,
    predicate: String,
    object: String,
    #[serde(default)]
    object_is_entity: bool,
}

async fn commit_extraction<S: Storage>(
    storage: &S,
    args: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let req: CommitArgs = serde_json::from_value(args.clone())?;

    let Some(mut item) = storage.get(&req.memory_id).await? else {
        anyhow::bail!("memory {} not found", req.memory_id);
    };

    // Race check: caller's contentHash must still match the current
    // content. If the memory was edited between extract_pending and
    // commit_extraction, reject — the agent should re-fetch.
    let current_hash = md5_hex(&item.content);
    if current_hash != req.content_hash {
        return Ok(text_result(&format!(
            "rejected: memory content changed since extract_pending (expected hash {}, now {}). Call memory_extract_pending again to re-fetch.",
            req.content_hash, current_hash,
        )));
    }

    let scope = item.scope;
    let entity_ctx_for = |default_type: String| EntityContext {
        platform_id: item.platform_id.clone(),
        project_id: item.project_id.clone(),
        scope,
        default_type,
    };

    // Resolve every named entity first; build name → id map so edges
    // can reference them by canonical name as the agent specified.
    let mut name_to_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut entities_created = 0usize;
    for ent in req.entities.iter() {
        let trimmed = ent.name.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entity_type = normalize_entity_type(&ent.type_);
        let resolved = resolve_or_create_entity(storage, trimmed, &entity_ctx_for(entity_type)).await?;
        name_to_id.insert(trimmed.to_lowercase(), resolved.id);
        entities_created += 1;
    }

    // Wire edges. Subject must be in the entities list; object can be
    // either an entity (must also be in the list) or a literal.
    let mut edges_created = 0usize;
    for edge in req.edges.iter() {
        let subj_key = edge.subject.trim().to_lowercase();
        let Some(subject_id) = name_to_id.get(&subj_key).cloned() else {
            continue;
        };
        let (object_id, object_literal) = if edge.object_is_entity {
            let object_key = edge.object.trim().to_lowercase();
            match name_to_id.get(&object_key).cloned() {
                Some(id) => (Some(id), None),
                None => continue,
            }
        } else {
            (None, Some(edge.object.trim().to_string()))
        };
        let predicate = normalize_predicate(&edge.predicate);
        if predicate.is_empty() {
            continue;
        }
        let now = Utc::now();
        let row = MemoryEdge {
            id: short_id(),
            created: now,
            updated: now,
            platform_id: item.platform_id.clone(),
            project_id: item.project_id.clone(),
            location_id: None,
            chatbot_id: None,
            chat_identity_id: None,
            scope,
            subject_id,
            predicate,
            object_id,
            object_literal,
            weight: 0.9,
            source_memory_id: Some(req.memory_id.clone()),
            metadata: serde_json::Value::Null,
            valid_from: now,
            valid_to: None,
        };
        storage.save_edge(&row).await?;
        edges_created += 1;
    }

    // Stamp the source memory's metadata so the same content isn't
    // re-extracted on the next extract_pending call.
    let now_iso = Utc::now().to_rfc3339();
    let mut meta = match &item.metadata {
        serde_json::Value::Object(_) => item.metadata.clone(),
        _ => json!({}),
    };
    if let Some(obj) = meta.as_object_mut() {
        obj.insert(
            "extractionContentHash".to_string(),
            serde_json::Value::String(current_hash),
        );
        obj.insert(
            "lastExtractionAt".to_string(),
            serde_json::Value::String(now_iso),
        );
    }
    item.metadata = meta;
    item.updated = Utc::now();
    storage.save(&item).await?;

    Ok(text_result(&format!(
        "extracted: {entities_created} entit{} resolved, {edges_created} edge{} written, source memory {} stamped",
        if entities_created == 1 { "y" } else { "ies" },
        if edges_created == 1 { "" } else { "s" },
        req.memory_id,
    )))
}

fn normalize_entity_type(t: &str) -> String {
    let lower = t.trim().to_lowercase();
    match lower.as_str() {
        "person" | "org" | "product" | "location" | "concept" | "event" | "document" => lower,
        _ => "other".to_string(),
    }
}

fn normalize_predicate(p: &str) -> String {
    p.trim()
        .to_lowercase()
        .replace(' ', "_")
        .replace('-', "_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

fn md5_hex(s: &str) -> String {
    // MD5 is fine here — change-detection only, not security.
    let digest = md5::compute(s.as_bytes());
    format!("{:x}", digest)
}

fn short_id() -> String {
    let raw = Uuid::now_v7().simple().to_string();
    raw.chars().take(21).collect()
}

fn memory_item_from_args(args: &serde_json::Value) -> anyhow::Result<MemoryItem> {
    let id = arg_str(args, "id")?.to_string();
    let platform_id = arg_str(args, "platformId")?.to_string();
    let type_ = arg_str(args, "type")?.to_string();
    let content = arg_str(args, "content")?.to_string();
    let scope = scope_from_str(arg_str(args, "scope")?)?;

    let mut item = MemoryItem::new(id, platform_id, type_, content, scope);
    item.project_id = arg_opt(args, "projectId");
    item.chatbot_id = arg_opt(args, "agentId");
    item.chat_identity_id = arg_opt(args, "userId");
    item.session_key = arg_opt(args, "sessionId");
    if let Some(v) = args.get("importance").and_then(|v| v.as_f64()) {
        item.importance = v as f32;
    }
    if let Some(v) = args.get("confidence").and_then(|v| v.as_f64()) {
        item.confidence = v as f32;
    }
    if let Some(v) = args.get("metadata") {
        if !v.is_null() {
            item.metadata = v.clone();
        }
    }
    // Event time. `MemoryItem::new` stamps `valid_from = now` (ingest time),
    // which is right for "I just learned this" and wrong for anything
    // back-dated. `valid_from` is the timestamp behavior mining buckets on, so
    // without this a backfill produces patterns describing the import run
    // rather than the events. `validFrom` is accepted as an alias for callers
    // that speak the storage field name.
    if let Some(ts) = args
        .get("occurredAt")
        .or_else(|| args.get("validFrom"))
        .and_then(|v| v.as_str())
    {
        item.valid_from = ts
            .parse::<chrono::DateTime<chrono::Utc>>()
            .map_err(|e| anyhow::anyhow!("occurredAt must be an RFC3339 timestamp: {e}"))?;
    }
    Ok(item)
}

fn filter_from_args(args: &serde_json::Value) -> anyhow::Result<MemoryFilter> {
    Ok(MemoryFilter {
        platform_id: arg_opt(args, "platformId"),
        project_id: arg_opt(args, "projectId"),
        agent_id: arg_opt(args, "agentId"),
        user_id: arg_opt(args, "userId"),
        session_id: arg_opt(args, "sessionId"),
        scope: args
            .get("scope")
            .and_then(|v| v.as_str())
            .map(scope_from_str)
            .transpose()?,
        kind: arg_opt(args, "type"),
        text_match: arg_opt(args, "query"),
        ..Default::default()
    })
}

fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: {key}"))
}

fn arg_opt(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn scope_from_str(s: &str) -> anyhow::Result<MemoryScope> {
    Ok(match s {
        "platform" => MemoryScope::Platform,
        "project" => MemoryScope::Project,
        "location" => MemoryScope::Location,
        "agent" => MemoryScope::Agent,
        "user" => MemoryScope::User,
        "session" => MemoryScope::Session,
        other => anyhow::bail!("invalid scope: {other}"),
    })
}

fn text_result(text: &str) -> serde_json::Value {
    // The MCP tools/call result shape: content array of typed items.
    json!({
        "content": [
            { "type": "text", "text": text }
        ]
    })
}
