// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Bidirectional sync between a local `SqliteStore` and the SaaS.
//!
//! ## Protocol
//!
//! The engine speaks to three endpoints on the SaaS:
//!
//!   POST /v1/projects/:projectId/memory/sync/push
//!   GET  /v1/projects/:projectId/memory/sync/pull
//!   POST /v1/memory/sync/bind         (platform-scoped; create new project)
//!
//! Push is per-project; the orchestrator groups `sync_status='pending'`
//! rows by `projectId` and POSTs one batch per project. Items kept their
//! client-generated ids, timestamps, and bi-temporal validity — the
//! server preserves all of that verbatim. On success, rows are marked
//! `synced`; on per-item rejection, marked `conflict` with the reason
//! logged.
//!
//! Pull is also per-project; the orchestrator reads `sync_state` for the
//! per-project cursor (`pull_cursor:<projectId>`), GETs items updated
//! after the cursor, upserts them locally with `sync_status='synced'`,
//! and advances the cursor. Conflict resolution is last-write-wins by
//! `updated` — the upsert in `SqliteStore::save()` replaces older rows.
//!
//! Bind is invoked when push encounters a memory whose `projectId` isn't
//! in the local bindings table AND the binding policy is `AutoCreate`.
//! The server creates a new project and returns `(platformId, projectId)`;
//! the orchestrator writes a binding and proceeds with the push.

use chrono::{DateTime, Utc};
use memory_core::{config::SyncConfig, sync_state_key, sync_status, MemoryItem, ProjectBinding};
use memory_storage::Storage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

pub use error::SyncError;

mod error {
    use memory_storage::StorageError;

    /// Errors that can interrupt a sync cycle. Individual per-item
    /// rejections from the server are NOT errors — they're recorded on the
    /// row as `sync_status='conflict'` and reported in the cycle stats.
    #[derive(Debug, thiserror::Error)]
    pub enum SyncError {
        #[error("http error: {0}")]
        Http(#[from] reqwest::Error),
        #[error("server returned {status}: {body}")]
        Server { status: u16, body: String },
        #[error("storage error: {0}")]
        Storage(#[from] StorageError),
        #[error("token validation failed (status {status})")]
        Auth { status: u16 },
        #[error("misconfigured: {0}")]
        Misconfigured(String),
    }
}

// ─── Wire types ──────────────────────────────────────────────────────
//
// Mirror of the SaaS-side TypeBox schemas in
// `packages/shared/src/lib/clawdbot/memory-sync.ts`. We use camelCase on
// the wire to match the SaaS conventions (it's TypeORM); local rust
// types use snake_case internally.

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PushRequest<'a> {
    device_id: &'a str,
    items: &'a [MemoryItemWire<'a>],
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResponse {
    pub accepted: Vec<String>,
    pub rejected: Vec<PushRejection>,
}

#[derive(Debug, Deserialize)]
pub struct PushRejection {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullResponse {
    pub items: Vec<MemoryItem>,
    pub cursor: String,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BindRequest<'a> {
    display_name: &'a str,
    cwd: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindResponse {
    pub platform_id: String,
    pub project_id: String,
    pub display_name: String,
    pub created: bool,
}

/// MemoryItem reshaped for the wire — same fields, but every camelCase
/// name the SaaS expects matches exactly. The local `MemoryItem` uses
/// snake_case internally; this struct does the rename at the boundary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryItemWire<'a> {
    id: &'a str,
    created: DateTime<Utc>,
    updated: DateTime<Utc>,
    platform_id: &'a str,
    project_id: Option<&'a str>,
    location_id: Option<&'a str>,
    chatbot_id: Option<&'a str>,
    chat_identity_id: Option<&'a str>,
    session_key: Option<&'a str>,
    #[serde(rename = "type")]
    type_: &'a str,
    content: &'a str,
    category: Option<&'a str>,
    importance: f32,
    source: Option<&'a str>,
    metadata: &'a serde_json::Value,
    scope: String,
    status: String,
    confidence: f32,
    impact: Option<String>,
    superseded_by_id: Option<&'a str>,
    confirmed_by_user_id: Option<&'a str>,
    confirmed_at: Option<DateTime<Utc>>,
    negative_rating_count: i32,
    valid_from: DateTime<Utc>,
    valid_to: Option<DateTime<Utc>>,
    learned_at: DateTime<Utc>,
    last_accessed_at: DateTime<Utc>,
    /// Server uses these for backfill bookkeeping; the engine doesn't
    /// know them, so always null on the wire.
    last_extraction_at: Option<DateTime<Utc>>,
    extraction_content_hash: Option<String>,
}

impl<'a> From<&'a MemoryItem> for MemoryItemWire<'a> {
    fn from(m: &'a MemoryItem) -> Self {
        Self {
            id: &m.id,
            created: m.created,
            updated: m.updated,
            platform_id: &m.platform_id,
            project_id: m.project_id.as_deref(),
            location_id: m.location_id.as_deref(),
            chatbot_id: m.chatbot_id.as_deref(),
            chat_identity_id: m.chat_identity_id.as_deref(),
            session_key: m.session_key.as_deref(),
            type_: &m.type_,
            content: &m.content,
            category: m.category.as_deref(),
            importance: m.importance,
            source: m.source.as_deref(),
            metadata: &m.metadata,
            scope: m.scope.as_str().to_string(),
            status: m.status.as_str().to_string(),
            confidence: m.confidence,
            impact: m.impact.map(|i| i.as_str().to_string()),
            superseded_by_id: m.superseded_by_id.as_deref(),
            confirmed_by_user_id: m.confirmed_by_user_id.as_deref(),
            confirmed_at: m.confirmed_at,
            negative_rating_count: m.negative_rating_count,
            valid_from: m.valid_from,
            valid_to: m.valid_to,
            learned_at: m.learned_at,
            last_accessed_at: m.last_accessed_at,
            last_extraction_at: None,
            extraction_content_hash: None,
        }
    }
}

// ─── HTTP client ─────────────────────────────────────────────────────

/// Thin wrapper over reqwest that holds the SaaS base URL + bearer
/// token. Reused across sync cycles — keeps the HTTP/2 connection pool
/// warm.
pub struct SyncClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl SyncClient {
    pub fn new(base_url: String, token: String) -> Result<Self, SyncError> {
        let base_url = base_url.trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(SyncError::Misconfigured("sync url is empty".into()));
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("memmesh/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(SyncError::Http)?;
        Ok(Self { base_url, token, http })
    }

    /// Cheap probe — hits `/v1/users/me` (or a similar inexpensive
    /// endpoint) to verify the bearer token. Used at sync startup so
    /// invalid tokens degrade to local-only mode instead of silently
    /// burning retry budget.
    pub async fn validate_token(&self) -> Result<(), SyncError> {
        let url = format!("{}/v1/users/me", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(SyncError::Auth { status: status.as_u16() })
        }
    }

    pub async fn push(
        &self,
        project_id: &str,
        device_id: &str,
        items: &[MemoryItem],
    ) -> Result<PushResponse, SyncError> {
        let wire: Vec<MemoryItemWire<'_>> = items.iter().map(Into::into).collect();
        let body = PushRequest {
            device_id,
            items: &wire,
        };
        let url = format!(
            "{}/v1/projects/{}/memory/sync/push",
            self.base_url, project_id
        );
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SyncError::Server {
                status: status.as_u16(),
                body,
            });
        }
        Ok(resp.json::<PushResponse>().await?)
    }

    pub async fn pull(
        &self,
        project_id: &str,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<PullResponse, SyncError> {
        let url = format!(
            "{}/v1/projects/{}/memory/sync/pull",
            self.base_url, project_id
        );
        let mut req = self.http.get(&url).bearer_auth(&self.token);
        // Single &mut chain pattern would borrow `cursor` immediately;
        // build query as a Vec instead.
        let mut q: Vec<(&str, String)> = Vec::with_capacity(2);
        if let Some(c) = cursor {
            if !c.is_empty() {
                q.push(("since", c.to_string()));
            }
        }
        q.push(("limit", limit.to_string()));
        req = req.query(&q);
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SyncError::Server {
                status: status.as_u16(),
                body,
            });
        }
        Ok(resp.json::<PullResponse>().await?)
    }

    pub async fn bind(&self, display_name: &str, cwd: &str) -> Result<BindResponse, SyncError> {
        let body = BindRequest { display_name, cwd };
        let url = format!("{}/v1/memory/sync/bind", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SyncError::Server {
                status: status.as_u16(),
                body,
            });
        }
        Ok(resp.json::<BindResponse>().await?)
    }
}

// ─── Orchestration ────────────────────────────────────────────────────

/// Stats from one push+pull cycle. Surfaced in logs and (eventually) the
/// desktop UI status panel.
#[derive(Debug, Default, Clone, Serialize)]
pub struct SyncCycleStats {
    pub pushed: u32,
    pub push_rejected: u32,
    pub pulled: u32,
    pub project_count: u32,
}

/// Maximum batch size for a single push. Tuned conservatively — large
/// enough that a backlog drains in a couple of cycles, small enough that
/// any single failure only re-tries a manageable batch.
const PUSH_BATCH_LIMIT: u32 = 100;
const PULL_BATCH_LIMIT: u32 = 200;

/// Stable device id — hash of `{hostname, OS username}`. Used by the
/// server for audit/observability, not for auth. Best-effort; falls back
/// to "unknown-device" if either component is unavailable.
pub fn device_id() -> String {
    let host = hostname().unwrap_or_else(|| "unknown-host".into());
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown-user".into());
    // Cheap deterministic hash — collisions don't matter (server only
    // logs this), but stability does.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    host.hash(&mut h);
    user.hash(&mut h);
    format!("dev-{:016x}", h.finish())
}

fn hostname() -> Option<String> {
    // Avoid bringing in another crate just for hostname; read /etc on
    // unix, fall back to env on windows.
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    if let Ok(h) = std::env::var("COMPUTERNAME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        let h = s.trim();
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }
    None
}

/// Run one full push+pull cycle.
///
/// Push: group pending items by projectId, POST one batch per project,
/// mark synced/conflict based on the per-item response.
///
/// Pull: for every binding in the engine's local table, GET items
/// updated after the per-project cursor, upsert with `sync_status='synced'`,
/// advance the cursor.
pub async fn run_cycle(
    storage: &(dyn Storage + Send + Sync),
    client: &SyncClient,
    sync_cfg: &SyncConfig,
) -> Result<SyncCycleStats, SyncError> {
    let mut stats = SyncCycleStats::default();
    let device = device_id();

    // ── PUSH ────────────────────────────────────────────────────────
    let pending = storage.list_pending_for_push(PUSH_BATCH_LIMIT).await?;
    if !pending.is_empty() {
        // Group by projectId. Items with no projectId can't be pushed
        // per-project — skip them and leave their sync_status='pending'
        // so they retry once a binding lands.
        let mut by_project: HashMap<String, Vec<MemoryItem>> = HashMap::new();
        let mut unbindable: Vec<String> = Vec::new();
        for item in pending {
            match item.project_id.clone() {
                Some(pid) if !pid.is_empty() => by_project.entry(pid).or_default().push(item),
                _ => unbindable.push(item.id.clone()),
            }
        }
        if !unbindable.is_empty() {
            debug!(
                count = unbindable.len(),
                "skipping items with no projectId (will retry once bound)"
            );
        }

        for (project_id, items) in by_project {
            // Resolve binding: in SaaS-connected mode every pushed
            // projectId should already match a SaaS project. If a row
            // carries a raw cwd-basename projectId (engine ran in
            // local-only mode before the user signed in), the auto-create
            // policy kicks in.
            let resolved = match resolve_project_for_push(
                storage,
                client,
                &project_id,
                &sync_cfg.platform_id,
                sync_cfg.binding_policy,
            )
            .await
            {
                Ok(Some(pid)) => pid,
                Ok(None) => {
                    debug!(project_id, "skipping unbound project this cycle");
                    continue;
                }
                // One project failing to resolve must not abort the whole
                // cycle — otherwise a single unbindable cwd blocks every
                // other project's push indefinitely. Same degrade-and-
                // retry contract as a failed push below.
                Err(e) => {
                    warn!(error = %e, project_id, "could not resolve project; skipping this cycle");
                    continue;
                }
            };

            stats.project_count += 1;
            match client.push(&resolved, &device, &items).await {
                Ok(resp) => {
                    let accepted = resp.accepted.len() as u32;
                    let rejected = resp.rejected.len() as u32;
                    stats.pushed += accepted;
                    stats.push_rejected += rejected;
                    if !resp.accepted.is_empty() {
                        storage
                            .set_sync_status(&resp.accepted, sync_status::SYNCED)
                            .await?;
                    }
                    if !resp.rejected.is_empty() {
                        let ids: Vec<String> =
                            resp.rejected.iter().map(|r| r.id.clone()).collect();
                        storage
                            .set_sync_status(&ids, sync_status::CONFLICT)
                            .await?;
                        for r in &resp.rejected {
                            warn!(id = r.id, reason = r.reason, "push rejected");
                        }
                    }
                    info!(
                        project = resolved,
                        accepted, rejected, "push batch complete"
                    );
                }
                Err(e) => {
                    warn!(error = %e, project = resolved, "push failed; will retry");
                    // Items stay 'pending'; next cycle picks them up.
                }
            }
        }
    }

    // ── PULL ────────────────────────────────────────────────────────
    let bindings = storage.list_bindings().await?;
    for b in bindings {
        if b.platform_id != sync_cfg.platform_id {
            continue;
        }
        let cursor_key = pull_cursor_key(&b.project_id);
        let cursor = storage.get_sync_state(&cursor_key).await?;
        match client
            .pull(&b.project_id, cursor.as_deref(), PULL_BATCH_LIMIT)
            .await
        {
            Ok(resp) => {
                let count = resp.items.len() as u32;
                stats.pulled += count;
                for item in resp.items {
                    // Upsert; SqliteStore::save() preserves the
                    // client/server bi-temporal fields. After upsert mark
                    // synced so we don't ping-pong it back on the next
                    // push.
                    storage.save(&item).await?;
                    storage
                        .set_sync_status(&[item.id], sync_status::SYNCED)
                        .await?;
                }
                storage.set_sync_state(&cursor_key, &resp.cursor).await?;
                if count > 0 {
                    info!(
                        project = b.project_id,
                        pulled = count,
                        has_more = resp.has_more,
                        "pull batch complete"
                    );
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    project = b.project_id,
                    "pull failed; will retry"
                );
            }
        }
    }

    storage
        .set_sync_state(sync_state_key::LAST_PULL_AT, &Utc::now().to_rfc3339())
        .await?;
    if stats.pushed > 0 {
        storage
            .set_sync_state(sync_state_key::LAST_PUSH_AT, &Utc::now().to_rfc3339())
            .await?;
    }

    Ok(stats)
}

/// Decide which SaaS-side projectId a batch of pending items should be
/// pushed under. `project_id` is the value stored on the local row —
/// usually already a SaaS id, but might be a raw cwd-basename if the row
/// was written before the user signed in.
///
/// Returns `None` if the row should be skipped this cycle (e.g.
/// hard-fail policy with no binding).
async fn resolve_project_for_push(
    storage: &(dyn Storage + Send + Sync),
    client: &SyncClient,
    project_id: &str,
    platform_id: &str,
    policy: memory_core::config::BindingPolicy,
) -> Result<Option<String>, SyncError> {
    // Heuristic: SaaS project IDs are 21-character ApIds (alphanumeric).
    // If `project_id` already looks like one, trust it. Otherwise it's a
    // local cwd-basename and we have to resolve it.
    if looks_like_apid(project_id) {
        return Ok(Some(project_id.to_string()));
    }

    // An explicit binding (from `memmesh bind`) always wins over the
    // policy fallback. Without this the CLI's binding was inert on the
    // push path, and auto-create was the only route to a SaaS id — which
    // a SERVICE-principal API key cannot take, since /v1/memory/sync/bind
    // is USER-only. Checking here lets an API key alone drive sync.
    if let Some(bound) = lookup_binding(storage, project_id, platform_id).await? {
        debug!(project_id, resolved = bound, "resolved via explicit binding");
        return Ok(Some(bound));
    }

    use memory_core::config::BindingPolicy::*;
    match policy {
        AutoCreate => {
            let resp = client.bind(project_id, project_id).await?;
            // Persist the binding locally so future writes don't trigger
            // bind() again.
            let now = Utc::now();
            storage
                .save_binding(&ProjectBinding {
                    cwd: project_id.to_string(),
                    platform_id: platform_id.to_string(),
                    project_id: resp.project_id.clone(),
                    created: now,
                    updated: now,
                })
                .await?;
            info!(
                local = project_id,
                resolved = resp.project_id,
                "auto-created SaaS project for unbound cwd"
            );
            Ok(Some(resp.project_id))
        }
        SoftFail => {
            debug!(project_id, "soft-fail: skipping push for unbound project");
            Ok(None)
        }
        HardFail => {
            warn!(project_id, "hard-fail: rejecting push for unbound project");
            Ok(None)
        }
    }
}

/// Find the SaaS project a local cwd-basename projectId is bound to.
///
/// Two binding shapes exist in the wild and both have to resolve:
/// - `memmesh bind` stores an absolute path as `cwd`, so the basename is
///   what a memory row's projectId will carry.
/// - the auto-create branch below stores the bare basename as `cwd`.
///
/// Exact match is tried first (cheap, covers auto-created rows), then a
/// basename scan. Bindings for another platform are ignored — a machine
/// may hold bindings for several orgs. `list_bindings` is ordered by
/// `updated DESC`, so the most recent binding wins a basename collision.
async fn lookup_binding(
    storage: &(dyn Storage + Send + Sync),
    project_id: &str,
    platform_id: &str,
) -> Result<Option<String>, SyncError> {
    if let Some(b) = storage.get_binding(project_id).await? {
        if b.platform_id == platform_id {
            return Ok(Some(b.project_id));
        }
    }
    let basename_matches = |cwd: &str| {
        std::path::Path::new(cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == project_id)
    };
    Ok(storage
        .list_bindings()
        .await?
        .into_iter()
        .find(|b| b.platform_id == platform_id && basename_matches(&b.cwd))
        .map(|b| b.project_id))
}

/// Heuristic — 21 char alphanumeric is the ApId shape activepieces uses.
fn looks_like_apid(s: &str) -> bool {
    s.len() == 21 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn pull_cursor_key(project_id: &str) -> String {
    format!("{}:{}", sync_state_key::PULL_CURSOR, project_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_storage::sqlite::SqliteStore;

    async fn store_with(bindings: &[(&str, &str, &str)]) -> SqliteStore {
        let s = SqliteStore::connect("sqlite::memory:").await.unwrap();
        s.migrate().await.unwrap();
        for (cwd, platform_id, project_id) in bindings {
            let now = Utc::now();
            s.save_binding(&ProjectBinding {
                cwd: (*cwd).to_string(),
                platform_id: (*platform_id).to_string(),
                project_id: (*project_id).to_string(),
                created: now,
                updated: now,
            })
            .await
            .unwrap();
        }
        s
    }

    /// `memmesh bind` stores an absolute path, but memory rows carry the
    /// cwd basename — the mismatch that made the CLI binding inert.
    #[tokio::test]
    async fn resolves_absolute_path_binding_by_basename() {
        let s = store_with(&[("/Users/x/Dev/memory-thinkfleet", "plat1", "SAAS_PROJECT_ID_001")]).await;
        let got = lookup_binding(&s, "memory-thinkfleet", "plat1").await.unwrap();
        assert_eq!(got.as_deref(), Some("SAAS_PROJECT_ID_001"));
    }

    /// The auto-create branch stores the bare basename as `cwd`.
    #[tokio::test]
    async fn resolves_bare_basename_binding() {
        let s = store_with(&[("memmesh", "plat1", "SAAS_PROJECT_ID_002")]).await;
        let got = lookup_binding(&s, "memmesh", "plat1").await.unwrap();
        assert_eq!(got.as_deref(), Some("SAAS_PROJECT_ID_002"));
    }

    /// A binding belonging to another org must not leak across platforms.
    #[tokio::test]
    async fn ignores_other_platform_bindings() {
        let s = store_with(&[("/Users/x/Dev/growth-os", "other-plat", "SAAS_PROJECT_ID_003")]).await;
        let got = lookup_binding(&s, "growth-os", "plat1").await.unwrap();
        assert_eq!(got, None);
    }

    #[tokio::test]
    async fn unbound_project_returns_none() {
        let s = store_with(&[]).await;
        let got = lookup_binding(&s, "never-bound", "plat1").await.unwrap();
        assert_eq!(got, None);
    }
}
