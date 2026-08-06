// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Observation entrypoint — runs heuristic extraction on a raw text input
//! and writes the resulting `MemoryItem`s through a Storage backend.
//!
//! Caller passes the text + light context; we generate IDs, fill in the
//! schema fields the extractor doesn't know about, and persist. Shared by
//! the MCP tool, the HTTP endpoint, and the CLI subcommand.

use chrono::{DateTime, Utc};
use memory_core::{
    extraction::{extract, ObserveContext, ObserveRole},
    MemoryItem, MemoryScope, MemoryStatus,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::quota::{ensure_under_cap, effective_cap, QuotaError};
use crate::{Storage, StorageError};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObserveRequest {
    pub text: String,
    #[serde(default)]
    pub role: Option<ObserveRole>,
    #[serde(default)]
    pub platform_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    /// When this happened *in the world*, as opposed to when it was observed.
    /// Defaults to now. Drives `valid_from` on every item extracted from this
    /// text — which is the timestamp behavior mining buckets on, so a backfill
    /// that leaves this unset produces patterns describing the import run
    /// rather than the events. Accepts `validFrom` as an alias.
    #[serde(default, alias = "validFrom")]
    pub occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObserveResponse {
    pub saved: Vec<MemoryItem>,
    /// How many extraction candidates the extractor found before any
    /// dedupe / save attempts.
    pub candidate_count: usize,
}

/// Run the heuristic extractor + persist anything found.
///
/// Reads the engine config to determine whether a free-tier cap applies.
/// If the cap is reached, returns `Ok` with `saved` empty and
/// `candidate_count` set so callers can surface "0 saved (cap reached)"
/// without raising. Storage errors still propagate.
pub async fn observe<S: Storage>(
    storage: &S,
    req: &ObserveRequest,
) -> Result<ObserveResponse, StorageError> {
    let ctx = ObserveContext {
        platform_id: req.platform_id.clone(),
        project_id: req.project_id.clone(),
        user_id: req.user_id.clone(),
        agent_id: req.agent_id.clone(),
        session_id: req.session_id.clone(),
        role: req.role,
    };
    let candidates = extract(&req.text, &ctx);
    let candidate_count = candidates.len();
    let mut saved = Vec::with_capacity(candidates.len());

    // Resolve effective cap once for the whole batch. Each candidate is
    // re-checked inside the loop so partial batches succeed cleanly.
    let cap = effective_cap(&memory_core::config::Config::load_or_default());

    for cand in candidates {
        // Cap check per item — accept some, reject the rest gracefully
        // rather than failing the whole observe call.
        match ensure_under_cap(storage, cap).await {
            Ok(()) => {}
            Err(QuotaError::CapReached { .. }) => break,
            Err(QuotaError::Storage(e)) => return Err(e),
        }
        let now = Utc::now();
        // Force scope from the user's context if it was supplied —
        // overrides the extractor's heuristic default. Prevents
        // "User prefers X" from landing in PROJECT scope just because
        // the regex picked that, when the caller explicitly wants USER.
        let scope = if cand.scope == MemoryScope::Project && req.project_id.is_none() {
            // Caller has no project — fall back to user scope so the
            // memory still lands somewhere useful.
            MemoryScope::User
        } else {
            cand.scope
        };

        let item = MemoryItem {
            id: short_id(),
            created: now,
            updated: now,
            platform_id: req
                .platform_id
                .clone()
                .unwrap_or_else(|| "local".to_string()),
            project_id: req.project_id.clone(),
            location_id: None,
            chatbot_id: req.agent_id.clone(),
            chat_identity_id: req.user_id.clone(),
            session_key: req.session_id.clone(),
            type_: cand.kind.to_string(),
            content: cand.content,
            category: None,
            importance: cand.importance,
            source: Some(format!("observe:{}", cand.reason)),
            metadata: serde_json::Value::Null,
            scope,
            status: MemoryStatus::Confirmed,
            confidence: 0.75, // heuristic — lower than user-stated facts
            impact: Some(cand.impact),
            superseded_by_id: None,
            confirmed_by_user_id: None,
            confirmed_at: None,
            negative_rating_count: 0,
            // Event time, defaulting to ingest time. `learned_at` stays `now`
            // regardless — that's the bi-temporal split: when it became true vs.
            // when we found out.
            valid_from: req.occurred_at.unwrap_or(now),
            valid_to: None,
            learned_at: now,
            last_accessed_at: now,
        };

        storage.save(&item).await?;

        // Generate + store the semantic embedding for this item so it's
        // retrievable by meaning, not just substring. No-op when embeddings are
        // disabled; never fatal (the memory is already persisted).
        crate::embedding::embed_and_store(storage, &item.id, &item.content).await;

        // Self-wiring graph: scan this item's content for entity
        // mentions and typed relationships, populate memory_entity +
        // memory_edge. Best-effort — a graph-extraction failure
        // shouldn't fail the whole observe call, since the primary
        // memory write has already succeeded. Log and continue.
        let graph_ctx = crate::graph_extractor::GraphContext {
            platform_id: item.platform_id.clone(),
            project_id: item.project_id.clone(),
            scope: item.scope,
            source_memory_id: item.id.clone(),
        };
        if let Err(e) =
            crate::graph_extractor::extract_and_wire(storage, &item.content, &graph_ctx).await
        {
            tracing::warn!(
                memory_id = %item.id,
                error = %e,
                "graph extraction failed (memory still saved)"
            );
        }

        // Bonus: extract entities from the ORIGINAL raw text too, not
        // just the normalized memory content. The original is the
        // user's actual message ("Met with @kevin at @acme today") —
        // catches mentions that the heuristic extractor stripped or
        // didn't promote into a fact. De-duped by alias on resolve,
        // so re-processing the same text on a follow-up observe is
        // idempotent.
        if let Err(e) =
            crate::graph_extractor::extract_and_wire(storage, &req.text, &graph_ctx).await
        {
            tracing::warn!(
                memory_id = %item.id,
                error = %e,
                "graph extraction on raw text failed (memory still saved)"
            );
        }

        saved.push(item);
    }

    Ok(ObserveResponse {
        saved,
        candidate_count,
    })
}

/// Compact 21-character ID compatible with the rest of the platform.
fn short_id() -> String {
    let raw = Uuid::now_v7().simple().to_string();
    raw.chars().take(21).collect()
}
