// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Storage abstraction. The `Storage` trait is the architectural cornerstone
//! of the engine — every downstream crate (server, mcp, sync, audit) talks to
//! it, never to a database driver directly.
//!
//! Two implementations:
//!   - `SqliteStore` (local desktop, embedded)
//!   - `PostgresStore` (SaaS, shared schema with the existing TypeScript API)
//!
//! Both implementations read/write the same logical schema. SQLite types are
//! chosen to round-trip cleanly with the Postgres types the TS API defines
//! (TEXT for varchar, BLOB for bytea, REAL for numeric, INTEGER unix-ms for
//! timestamptz).

pub mod budget;
pub mod entity_resolver;
pub mod error;
pub mod graph_extractor;
pub mod observe;
pub mod query;
pub mod quota;
pub mod validate;

#[cfg(feature = "postgres")]
pub mod postgres;

#[cfg(feature = "sqlite")]
pub mod sqlite;

use async_trait::async_trait;
use memory_core::{
    Accumulator, AttributeDef, Contact, ContactEvent, IngestReport, MemoryAuditEvent, MemoryBlock,
    MemoryEdge, MemoryEntity, MemoryFeedback, MemoryItem, ProjectBinding, TypedObservation,
    UserProfile,
};

pub use error::StorageError;
pub use query::{
    AccumulatorKey, AttributeDefFilter, AuditFilter, BlockFilter, ContactEventFilter,
    ContactFilter, EdgeFilter, EntityFilter, FeedbackFilter, MemoryFilter, MemoryQuery,
    MemoryStats, ScoredMemoryItem, TypedObservationFilter,
};

/// The single interface every other crate uses to read or write memory.
/// Implementations live in this crate (`SqliteStore`, `PostgresStore`).
///
/// All methods are async + fallible. Trait is `Send + Sync` so it can be
/// shared across the tokio runtime safely.
///
/// Methods are grouped by table to keep navigation manageable as the trait
/// grows. v1 covers `clawdbot_memory_item`, `memory_audit_event`,
/// `agent_memory_feedback`. The remaining six schema-mirrored tables land
/// in follow-up methods on this trait.
#[async_trait]
pub trait Storage: Send + Sync + 'static {
    // ── clawdbot_memory_item ────────────────────────────────

    /// Insert or upsert a memory item.
    async fn save(&self, item: &MemoryItem) -> Result<(), StorageError>;

    /// Fetch a memory item by primary key.
    async fn get(&self, id: &str) -> Result<Option<MemoryItem>, StorageError>;

    /// Query memory items matching the filter. Sorting + ranking happens
    /// upstream in `memory-core::scoring`; this just returns raw rows.
    async fn query(&self, q: &MemoryQuery) -> Result<Vec<MemoryItem>, StorageError>;

    /// Mark an item superseded by a newer one (no physical delete).
    async fn supersede(&self, id: &str, by_id: &str) -> Result<(), StorageError>;

    /// Delete a memory item.
    ///
    /// - `hard = false` (default): soft-delete via `status = 'rejected'`.
    ///   Row stays in place so sync can propagate the rejection signal to
    ///   peer stores. This is the right choice for almost every caller.
    /// - `hard = true`: physical SQL DELETE. Loses sync traceability;
    ///   only use for operator-driven cleanups, GDPR right-to-forget, etc.
    async fn delete(&self, id: &str, hard: bool) -> Result<(), StorageError>;

    /// Bump `last_accessed_at` for an item. Called by retrieval paths to
    /// drive the recency component of the hybrid scorer.
    async fn touch(&self, id: &str) -> Result<(), StorageError>;

    // ── memory_audit_event ──────────────────────────────────

    /// Append an audit event. The table is append-only — there's no
    /// `update_audit` by design.
    async fn record_audit(&self, event: &MemoryAuditEvent) -> Result<(), StorageError>;

    /// Query audit events for dashboards / Shield export. Ordered newest
    /// first. Caller paginates via limit/offset.
    async fn query_audit(&self, q: &AuditFilter) -> Result<Vec<MemoryAuditEvent>, StorageError>;

    // ── agent_memory_feedback ───────────────────────────────

    /// Record a rating against a memory item. Negative ratings also
    /// atomically increment the parent memory's `negativeRatingCount`.
    async fn record_feedback(&self, fb: &MemoryFeedback) -> Result<(), StorageError>;

    /// List feedback rows matching the filter (typically by memoryId).
    async fn query_feedback(&self, q: &FeedbackFilter)
        -> Result<Vec<MemoryFeedback>, StorageError>;

    // ── clawdbot_contact ────────────────────────────────────

    /// Insert or upsert a contact record.
    async fn save_contact(&self, contact: &Contact) -> Result<(), StorageError>;

    /// Fetch a contact by id.
    async fn get_contact(&self, id: &str) -> Result<Option<Contact>, StorageError>;

    /// List contacts matching the filter.
    async fn query_contacts(&self, q: &ContactFilter) -> Result<Vec<Contact>, StorageError>;

    // ── clawdbot_contact_event ──────────────────────────────

    /// Insert or upsert a contact event (lattice substrate).
    async fn save_contact_event(&self, event: &ContactEvent) -> Result<(), StorageError>;

    /// Fetch a contact event by id.
    async fn get_contact_event(&self, id: &str) -> Result<Option<ContactEvent>, StorageError>;

    /// List events for pattern detection. Default sort: occurredAt DESC.
    async fn query_contact_events(
        &self,
        q: &ContactEventFilter,
    ) -> Result<Vec<ContactEvent>, StorageError>;

    /// Count contact events in a project scope since a timestamp.
    ///
    /// Powers the per-tenant monthly events-ingested cap. The
    /// `clawdbot_contact_event` table is scoped by `projectId` (it has
    /// no `platformId` column), so the count is taken at project grain —
    /// the finest tenant scope the events table exposes. `since` is the
    /// inclusive lower bound on `occurredAt` (start of the current UTC
    /// month for the cap check). Returns a raw COUNT(*); the caller
    /// compares it to the cap and fails OPEN on any error.
    async fn count_contact_events_since(
        &self,
        project_id: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, StorageError>;

    // ── agent_user_profile ──────────────────────────────────

    /// Upsert a profile. Uniqueness enforced on (chatIdentityId, projectId)
    /// — the same identity can have different profiles per project.
    async fn save_user_profile(&self, profile: &UserProfile) -> Result<(), StorageError>;

    /// Fetch a profile by primary key.
    async fn get_user_profile(&self, id: &str) -> Result<Option<UserProfile>, StorageError>;

    /// Fetch a profile by the natural key (chatIdentityId + projectId).
    async fn get_user_profile_by_identity(
        &self,
        chat_identity_id: &str,
        project_id: &str,
    ) -> Result<Option<UserProfile>, StorageError>;

    // ── agent_memory_block ──────────────────────────────────

    /// Upsert a labeled scratchpad block.
    async fn save_block(&self, block: &MemoryBlock) -> Result<(), StorageError>;

    /// Fetch a block by id.
    async fn get_block(&self, id: &str) -> Result<Option<MemoryBlock>, StorageError>;

    /// List blocks matching the filter.
    async fn query_blocks(&self, q: &BlockFilter) -> Result<Vec<MemoryBlock>, StorageError>;

    // ── memory_entity ───────────────────────────────────────

    /// Upsert a knowledge-graph entity.
    async fn save_entity(&self, entity: &MemoryEntity) -> Result<(), StorageError>;

    /// Fetch an entity by id.
    async fn get_entity(&self, id: &str) -> Result<Option<MemoryEntity>, StorageError>;

    /// List entities matching the filter.
    async fn query_entities(&self, q: &EntityFilter) -> Result<Vec<MemoryEntity>, StorageError>;

    // ── memory_edge ─────────────────────────────────────────

    /// Upsert a knowledge-graph edge. Exactly one of `object_id` or
    /// `object_literal` must be set — enforced by a CHECK constraint on
    /// both backends.
    async fn save_edge(&self, edge: &MemoryEdge) -> Result<(), StorageError>;

    /// Fetch an edge by id.
    async fn get_edge(&self, id: &str) -> Result<Option<MemoryEdge>, StorageError>;

    /// List edges matching the filter (subject/object/predicate + scope).
    async fn query_edges(&self, q: &EdgeFilter) -> Result<Vec<MemoryEdge>, StorageError>;

    // ── project_bindings ────────────────────────────────────
    //
    // Per-machine mapping from a local cwd to a SaaS (platformId, projectId).
    // Used by the sync layer to resolve every memory write to the right
    // SaaS-side project. NOT itself synced — cwd paths are per-machine.

    /// Upsert a binding. Refreshes `updated` on every write.
    async fn save_binding(&self, binding: &ProjectBinding) -> Result<(), StorageError>;

    /// Exact-match lookup on `cwd`. Returns `None` if no binding exists.
    /// Hierarchical lookup (walk up the dir tree) lives one layer up — this
    /// method does exactly what its name says.
    async fn get_binding(&self, cwd: &str) -> Result<Option<ProjectBinding>, StorageError>;

    /// All bindings on this machine. Ordered by `updated` desc.
    async fn list_bindings(&self) -> Result<Vec<ProjectBinding>, StorageError>;

    /// Remove a binding. Idempotent: ok if the row doesn't exist.
    async fn remove_binding(&self, cwd: &str) -> Result<(), StorageError>;

    // ── sync_state ──────────────────────────────────────────
    //
    // Free-form key/value store for sync bookkeeping (pull cursor, last
    // push/pull timestamps, cached plan tier, etc). See
    // `memory_core::sync_state_key` for the documented set of keys.

    /// Read a value by key. Returns `None` if unset.
    async fn get_sync_state(&self, key: &str) -> Result<Option<String>, StorageError>;

    /// Upsert a value. Bumps `updated` automatically.
    async fn set_sync_state(&self, key: &str, value: &str) -> Result<(), StorageError>;

    // ── quotas / diagnostics ────────────────────────────────

    /// Total count of memory items (any status). Used by the free-tier cap
    /// check before save. Cheap on SQLite given the size of free-tier DBs;
    /// optimize if it ever shows up in profiles.
    async fn count_items(&self) -> Result<i64, StorageError>;

    // ── sync push/pull helpers ──────────────────────────────
    //
    // Used by `memory-sync` to drive the bidirectional push/pull loop
    // against a remote SaaS. The sync crate intentionally drives all
    // I/O through these methods so the same protocol works on top of
    // either storage backend.

    /// Fetch memory items that still need to be pushed to the remote.
    /// Returns rows with `sync_status = 'pending'`, ordered by
    /// `updated` ascending so the smallest write wins on the remote
    /// side when conflicts merge. Caller controls batch size.
    async fn list_pending_for_push(&self, limit: u32) -> Result<Vec<MemoryItem>, StorageError>;

    /// Bulk update `sync_status` for a set of ids. Used by the push
    /// loop to mark items `synced` after a successful POST, or
    /// `conflict` after a server-side rejection. No-op on empty ids.
    async fn set_sync_status(&self, ids: &[String], status: &str) -> Result<(), StorageError>;

    // ── vector index (clawdbot_memory_item.embedding) ──────
    //
    // Embeddings are a *local, derived index*: regenerated per-backend from
    // `content`, written via `set_embedding`, and never synced — they are not
    // part of `MemoryItem`, so the sync wire format can't carry them, and
    // `save`'s upsert leaves the `embedding` column untouched. sqlite stores
    // the vector as a BLOB and ranks by brute-force cosine in Rust; postgres
    // uses the pgvector `vector` column + HNSW cosine index.

    /// Upsert the embedding for an item. Writes only the `embedding` column
    /// (does not bump `updated`, so it never triggers a re-sync). An empty
    /// vector clears it back to NULL.
    async fn set_embedding(&self, id: &str, vector: &[f32]) -> Result<(), StorageError>;

    /// Items matching `filter` that have no embedding yet — the backfill
    /// work-list. Newest first, capped at `limit`.
    async fn items_missing_embedding(
        &self,
        filter: &MemoryFilter,
        limit: u32,
    ) -> Result<Vec<MemoryItem>, StorageError>;

    /// k-nearest memories to `query` by cosine similarity, restricted to rows
    /// matching `filter`. Returns items paired with similarity (`-1..=1`),
    /// closest first. An empty `query` or `limit` of 0 returns no rows
    /// (semantic search disabled).
    async fn vector_search(
        &self,
        query: &[f32],
        filter: &MemoryFilter,
        limit: u32,
    ) -> Result<Vec<ScoredMemoryItem>, StorageError>;

    // ── observability ───────────────────────────────────────

    /// Aggregate dashboard counts scoped to `filter` (platform_id + optional
    /// project_id): totals, breakdowns by scope/status/type, embedding (vector)
    /// coverage, and behavior-pattern count. Cheap `COUNT`/`GROUP BY` queries.
    async fn memory_stats(&self, filter: &MemoryFilter) -> Result<MemoryStats, StorageError>;

    // ── typed attributes (memory_typed_observation + accumulator) ──
    //
    // The typed-attribute layer stores structured/numeric measurements the
    // engine can reason over (range queries, thresholds, trajectories) rather
    // than opaque metadata JSON. Ingest is batch + idempotent: each call
    // validates rows against the registry, stores them (accepted or
    // quarantined), and atomically folds accepted rows into per-subject
    // running accumulators — exactly once, even under at-least-once delivery
    // and concurrent writers (guarded by `memory_ingest_ledger`).

    /// Upsert an attribute definition into the per-project registry.
    async fn upsert_attribute_def(&self, def: &AttributeDef) -> Result<(), StorageError>;

    /// List attribute definitions matching the filter.
    async fn query_attribute_defs(
        &self,
        filter: &AttributeDefFilter,
    ) -> Result<Vec<AttributeDef>, StorageError>;

    /// Validate + persist a batch of typed observations and fold the accepted
    /// ones into their accumulators. Returns counts of accepted / quarantined /
    /// duplicate rows. Idempotent: an observation id already in the ingest
    /// ledger is skipped (counted as a duplicate, not re-accumulated).
    async fn save_typed_observations(
        &self,
        observations: &[TypedObservation],
    ) -> Result<IngestReport, StorageError>;

    /// Query raw typed observations (time-series / range scans).
    async fn query_typed_observations(
        &self,
        filter: &TypedObservationFilter,
    ) -> Result<Vec<TypedObservation>, StorageError>;

    /// Read a single subject+attribute accumulator (running stats), or `None`
    /// if no accepted observation has ever landed for it.
    async fn get_accumulator(
        &self,
        key: &AccumulatorKey,
    ) -> Result<Option<Accumulator>, StorageError>;

    // ── schema + diagnostics ────────────────────────────────

    /// Run pending schema migrations. Returns the schema version after.
    async fn migrate(&self) -> Result<i64, StorageError>;

    /// Backend identifier — used by audit/sync layers for tagging events.
    fn backend(&self) -> &'static str;
}
