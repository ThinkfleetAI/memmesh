// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! SqliteStore — local desktop backend. Schema mirrors the SaaS Postgres
//! tables column-for-column so the sync engine reconciles rows directly,
//! not via field-name translation.
//!
//! Type round-trip rules between Postgres ↔ SQLite:
//!   `varchar`         ↔ `TEXT`
//!   `timestamptz`     ↔ `TEXT` ISO-8601 UTC ("2026-05-15T11:22:33.456Z")
//!   `real`            ↔ `REAL`
//!   `integer`         ↔ `INTEGER`
//!   `boolean`         ↔ `INTEGER` (0 / 1)
//!   `jsonb`           ↔ `TEXT` (JSON-encoded)
//!   `vector(N)`       ↔ `BLOB` (raw f32 little-endian) — vector index built
//!                      via sqlite-vec virtual table in a later migration.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memory_core::{
    Contact, ContactEvent, FeedbackKind, MemoryAuditEvent, MemoryBlock, MemoryEdge, MemoryEntity,
    MemoryFeedback, MemoryImpact, MemoryItem, MemoryScope, MemoryStatus, ProjectBinding,
    UserProfile,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::collections::BTreeMap;
use std::str::FromStr;

use crate::query::{
    AccumulatorKey, AttributeDefFilter, AuditFilter, BlockFilter, ContactEventFilter,
    ContactFilter, EdgeFilter, EntityFilter, FeedbackFilter, MemoryFilter, MemoryQuery,
    MemoryStats, ScoredMemoryItem, TypedObservationFilter,
};
use crate::{Storage, StorageError};
use memory_core::{Accumulator, AttributeDef, IngestReport, TypedObservation};

/// Embedded migrations under `migrations/sqlite/`. The numeric prefix is the
/// migration version sqlx tracks in the `_sqlx_migrations` table.
static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/sqlite");

pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Open (or create) a SQLite database at the given URL. Use
    /// `"sqlite::memory:"` for an in-memory store (tests).
    ///
    /// Foreign keys are enabled per-connection — SQLite defaults to off.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let opts = SqliteConnectOptions::from_str(url)
            .map_err(|e| StorageError::Other(anyhow::anyhow!(e)))?
            .create_if_missing(true)
            .foreign_keys(true)
            // WAL gives much better concurrency than the default rollback
            // journal — important for the local desktop case where the
            // server, CLI, and sync daemon may all hit the same file.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl Storage for SqliteStore {
    async fn save(&self, item: &MemoryItem) -> Result<(), StorageError> {
        // UPSERT via `INSERT ... ON CONFLICT DO UPDATE`. Sets updated = now
        // automatically on every write so the sync engine has a reliable
        // last-write-wins fallback signal.
        sqlx::query(
            r#"
            INSERT INTO clawdbot_memory_item (
                id, created, updated,
                platformId, projectId, locationId, chatbotId, chatIdentityId, sessionKey,
                type, content, category, importance, source, metadata,
                scope, status, confidence, impact,
                supersededById, confirmedByUserId, confirmedAt, negativeRatingCount,
                validFrom, validTo, learnedAt, lastAccessedAt
            ) VALUES (
                ?, ?, ?,
                ?, ?, ?, ?, ?, ?,
                ?, ?, ?, ?, ?, ?,
                ?, ?, ?, ?,
                ?, ?, ?, ?,
                ?, ?, ?, ?
            )
            ON CONFLICT(id) DO UPDATE SET
                updated              = excluded.updated,
                platformId           = excluded.platformId,
                projectId            = excluded.projectId,
                locationId           = excluded.locationId,
                chatbotId            = excluded.chatbotId,
                chatIdentityId       = excluded.chatIdentityId,
                sessionKey           = excluded.sessionKey,
                type                 = excluded.type,
                content              = excluded.content,
                category             = excluded.category,
                importance           = excluded.importance,
                source               = excluded.source,
                metadata             = excluded.metadata,
                scope                = excluded.scope,
                status               = excluded.status,
                confidence           = excluded.confidence,
                impact               = excluded.impact,
                supersededById       = excluded.supersededById,
                confirmedByUserId    = excluded.confirmedByUserId,
                confirmedAt          = excluded.confirmedAt,
                negativeRatingCount  = excluded.negativeRatingCount,
                validFrom            = excluded.validFrom,
                validTo              = excluded.validTo,
                learnedAt            = excluded.learnedAt,
                lastAccessedAt       = excluded.lastAccessedAt
            "#,
        )
        .bind(&item.id)
        .bind(iso(item.created))
        .bind(iso(Utc::now())) // updated bumped on every write
        .bind(&item.platform_id)
        .bind(&item.project_id)
        .bind(&item.location_id)
        .bind(&item.chatbot_id)
        .bind(&item.chat_identity_id)
        .bind(&item.session_key)
        .bind(&item.type_)
        .bind(&item.content)
        .bind(&item.category)
        .bind(item.importance)
        .bind(&item.source)
        .bind(serde_json::to_string(&item.metadata).unwrap_or_else(|_| "null".into()))
        .bind(item.scope.as_str())
        .bind(item.status.as_str())
        .bind(item.confidence)
        .bind(item.impact.map(|i| i.as_str()))
        .bind(&item.superseded_by_id)
        .bind(&item.confirmed_by_user_id)
        .bind(item.confirmed_at.map(iso))
        .bind(item.negative_rating_count)
        .bind(iso(item.valid_from))
        .bind(item.valid_to.map(iso))
        .bind(iso(item.learned_at))
        .bind(iso(item.last_accessed_at))
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryItem>, StorageError> {
        let row = sqlx::query("SELECT * FROM clawdbot_memory_item WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        row.map(row_to_memory_item).transpose()
    }

    async fn query(&self, q: &MemoryQuery) -> Result<Vec<MemoryItem>, StorageError> {
        // Build a parameterised WHERE clause from the filter. Keep it
        // straightforward — no SQL builder dependency for the v1 backend.
        // Hybrid ranking happens upstream in `memory-core::scoring`.
        let mut sql = String::from("SELECT * FROM clawdbot_memory_item WHERE 1=1");
        let (clause, binds) = sqlite_filter_clause(&q.filter);
        sql.push_str(&clause);

        sql.push_str(" ORDER BY learnedAt DESC");
        if let Some(limit) = q.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = q.offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }

        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_memory_item).collect()
    }

    async fn supersede(&self, id: &str, by_id: &str) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await?;
        // Ensure the superseding item actually exists.
        let exists: Option<(String,)> =
            sqlx::query_as("SELECT id FROM clawdbot_memory_item WHERE id = ?")
                .bind(by_id)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(StorageError::NotFound(by_id.to_string()));
        }
        let result = sqlx::query(
            "UPDATE clawdbot_memory_item
             SET status = 'superseded',
                 supersededById = ?,
                 updated = ?
             WHERE id = ?",
        )
        .bind(by_id)
        .bind(iso(Utc::now()))
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(id.to_string()));
        }
        tx.commit().await?;
        Ok(())
    }

    async fn touch(&self, id: &str) -> Result<(), StorageError> {
        let result = sqlx::query("UPDATE clawdbot_memory_item SET lastAccessedAt = ? WHERE id = ?")
            .bind(iso(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn delete(&self, id: &str, hard: bool) -> Result<(), StorageError> {
        let result = if hard {
            sqlx::query("DELETE FROM clawdbot_memory_item WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await?
        } else {
            sqlx::query(
                "UPDATE clawdbot_memory_item
                 SET status = 'rejected', updated = ?
                 WHERE id = ?",
            )
            .bind(iso(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?
        };
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn record_audit(&self, event: &MemoryAuditEvent) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO memory_audit_event (
                id, created, platformId, projectId, actor, eventType,
                query, scope, memoryIds, resultCount, metadata
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&event.id)
        .bind(iso(event.created))
        .bind(&event.platform_id)
        .bind(&event.project_id)
        .bind(&event.actor)
        .bind(&event.event_type)
        .bind(&event.query)
        .bind(&event.scope)
        .bind(&event.memory_ids)
        .bind(event.result_count)
        .bind(serde_json::to_string(&event.metadata).unwrap_or_else(|_| "null".into()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn query_audit(&self, q: &AuditFilter) -> Result<Vec<MemoryAuditEvent>, StorageError> {
        let mut sql = String::from("SELECT * FROM memory_audit_event WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();

        if let Some(s) = &q.platform_id {
            sql.push_str(" AND platformId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            sql.push_str(" AND projectId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.actor {
            sql.push_str(" AND actor = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.event_type {
            sql.push_str(" AND eventType = ?");
            binds.push(s.clone());
        }
        sql.push_str(" ORDER BY created DESC");
        if let Some(limit) = q.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = q.offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }

        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_audit_event).collect()
    }

    async fn record_feedback(&self, fb: &MemoryFeedback) -> Result<(), StorageError> {
        // Inserting feedback + bumping the parent memory's
        // negativeRatingCount needs to be atomic; otherwise a race could
        // double-count or count without writing the row.
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            INSERT INTO agent_memory_feedback (
                id, created, updated, memoryId, responseId, rating,
                comment, createdByUserId
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&fb.id)
        .bind(iso(fb.created))
        .bind(iso(Utc::now()))
        .bind(&fb.memory_id)
        .bind(&fb.response_id)
        .bind(match fb.rating {
            FeedbackKind::Positive => "positive",
            FeedbackKind::Negative => "negative",
        })
        .bind(&fb.comment)
        .bind(&fb.created_by_user_id)
        .execute(&mut *tx)
        .await?;

        if matches!(fb.rating, FeedbackKind::Negative) {
            sqlx::query(
                "UPDATE clawdbot_memory_item
                 SET negativeRatingCount = negativeRatingCount + 1,
                     updated = ?
                 WHERE id = ?",
            )
            .bind(iso(Utc::now()))
            .bind(&fb.memory_id)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    async fn query_feedback(
        &self,
        q: &FeedbackFilter,
    ) -> Result<Vec<MemoryFeedback>, StorageError> {
        let mut sql = String::from("SELECT * FROM agent_memory_feedback WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();

        if let Some(s) = &q.memory_id {
            sql.push_str(" AND memoryId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.created_by_user_id {
            sql.push_str(" AND createdByUserId = ?");
            binds.push(s.clone());
        }
        if let Some(rating) = q.rating {
            sql.push_str(" AND rating = ?");
            binds.push(
                match rating {
                    FeedbackKind::Positive => "positive",
                    FeedbackKind::Negative => "negative",
                }
                .to_string(),
            );
        }
        sql.push_str(" ORDER BY created DESC");
        if let Some(limit) = q.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = q.offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }

        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_feedback).collect()
    }

    async fn save_contact(&self, c: &Contact) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO clawdbot_contact (
                id, created, updated, chatbotId, projectId, name, phone, email,
                relationship, isEmergency, priority, notes, metadata, externalIds,
                tags, segment, lifetimeValue, lastInteractionAt, chatIdentityId
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                updated           = excluded.updated,
                chatbotId         = excluded.chatbotId,
                projectId         = excluded.projectId,
                name              = excluded.name,
                phone             = excluded.phone,
                email             = excluded.email,
                relationship      = excluded.relationship,
                isEmergency       = excluded.isEmergency,
                priority          = excluded.priority,
                notes             = excluded.notes,
                metadata          = excluded.metadata,
                externalIds       = excluded.externalIds,
                tags              = excluded.tags,
                segment           = excluded.segment,
                lifetimeValue     = excluded.lifetimeValue,
                lastInteractionAt = excluded.lastInteractionAt,
                chatIdentityId    = excluded.chatIdentityId
            "#,
        )
        .bind(&c.id)
        .bind(iso(c.created))
        .bind(iso(Utc::now()))
        .bind(&c.chatbot_id)
        .bind(&c.project_id)
        .bind(&c.name)
        .bind(&c.phone)
        .bind(&c.email)
        .bind(&c.relationship)
        .bind(c.is_emergency as i32)
        .bind(c.priority)
        .bind(&c.notes)
        .bind(json_or_null(&c.metadata))
        .bind(json_or_null(&c.external_ids))
        .bind(json_or_default(&c.tags, "[]"))
        .bind(&c.segment)
        .bind(c.lifetime_value)
        .bind(c.last_interaction_at.map(iso))
        .bind(&c.chat_identity_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_contact(&self, id: &str) -> Result<Option<Contact>, StorageError> {
        let row = sqlx::query("SELECT * FROM clawdbot_contact WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_contact).transpose()
    }

    async fn query_contacts(&self, q: &ContactFilter) -> Result<Vec<Contact>, StorageError> {
        let mut sql = String::from("SELECT * FROM clawdbot_contact WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = &q.chatbot_id {
            sql.push_str(" AND chatbotId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            sql.push_str(" AND projectId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.chat_identity_id {
            sql.push_str(" AND chatIdentityId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.email {
            sql.push_str(" AND email = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.segment {
            sql.push_str(" AND segment = ?");
            binds.push(s.clone());
        }
        sql.push_str(" ORDER BY updated DESC");
        if let Some(limit) = q.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = q.offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }
        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_contact).collect()
    }

    async fn save_contact_event(&self, e: &ContactEvent) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO clawdbot_contact_event (
                id, created, updated, contactId, projectId, chatbotId,
                eventType, title, description, eventData, occurredAt, source,
                engagementRuleId, engagementExecutionId
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                updated                = excluded.updated,
                contactId              = excluded.contactId,
                projectId              = excluded.projectId,
                chatbotId              = excluded.chatbotId,
                eventType              = excluded.eventType,
                title                  = excluded.title,
                description            = excluded.description,
                eventData              = excluded.eventData,
                occurredAt             = excluded.occurredAt,
                source                 = excluded.source,
                engagementRuleId       = excluded.engagementRuleId,
                engagementExecutionId  = excluded.engagementExecutionId
            "#,
        )
        .bind(&e.id)
        .bind(iso(e.created))
        .bind(iso(Utc::now()))
        .bind(&e.contact_id)
        .bind(&e.project_id)
        .bind(&e.chatbot_id)
        .bind(&e.event_type)
        .bind(&e.title)
        .bind(&e.description)
        .bind(json_or_null(&e.event_data))
        .bind(iso(e.occurred_at))
        .bind(&e.source)
        .bind(&e.engagement_rule_id)
        .bind(&e.engagement_execution_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_contact_event(&self, id: &str) -> Result<Option<ContactEvent>, StorageError> {
        let row = sqlx::query("SELECT * FROM clawdbot_contact_event WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_contact_event).transpose()
    }

    async fn query_contact_events(
        &self,
        q: &ContactEventFilter,
    ) -> Result<Vec<ContactEvent>, StorageError> {
        let mut sql = String::from("SELECT * FROM clawdbot_contact_event WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = &q.contact_id {
            sql.push_str(" AND contactId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            sql.push_str(" AND projectId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.event_type {
            sql.push_str(" AND eventType = ?");
            binds.push(s.clone());
        }
        if let Some(ts) = q.since {
            sql.push_str(" AND occurredAt >= ?");
            binds.push(iso(ts));
        }
        if let Some(ts) = q.until {
            sql.push_str(" AND occurredAt < ?");
            binds.push(iso(ts));
        }
        sql.push_str(" ORDER BY occurredAt DESC");
        if let Some(limit) = q.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = q.offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }
        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_contact_event).collect()
    }

    async fn count_contact_events_since(
        &self,
        project_id: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, StorageError> {
        // Per-tenant monthly events cap. Scoped by projectId (the events
        // table has no platformId). Timestamps are stored as ISO strings
        // on SQLite, so bind `since` via iso() to compare lexically.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM clawdbot_contact_event \
             WHERE projectId = ? AND occurredAt >= ?",
        )
        .bind(project_id)
        .bind(iso(since))
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    async fn save_user_profile(&self, p: &UserProfile) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO agent_user_profile (
                id, created, updated, chatIdentityId, projectId, platformId,
                profile, profileVersion, lastUpdatedFromSessionId
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                updated                  = excluded.updated,
                chatIdentityId           = excluded.chatIdentityId,
                projectId                = excluded.projectId,
                platformId               = excluded.platformId,
                profile                  = excluded.profile,
                profileVersion           = excluded.profileVersion,
                lastUpdatedFromSessionId = excluded.lastUpdatedFromSessionId
            "#,
        )
        .bind(&p.id)
        .bind(iso(p.created))
        .bind(iso(Utc::now()))
        .bind(&p.chat_identity_id)
        .bind(&p.project_id)
        .bind(&p.platform_id)
        .bind(json_or_default(&p.profile, "{}"))
        .bind(p.profile_version)
        .bind(&p.last_updated_from_session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_user_profile(&self, id: &str) -> Result<Option<UserProfile>, StorageError> {
        let row = sqlx::query("SELECT * FROM agent_user_profile WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_user_profile).transpose()
    }

    async fn get_user_profile_by_identity(
        &self,
        chat_identity_id: &str,
        project_id: &str,
    ) -> Result<Option<UserProfile>, StorageError> {
        let row = sqlx::query(
            "SELECT * FROM agent_user_profile
             WHERE chatIdentityId = ? AND projectId = ?",
        )
        .bind(chat_identity_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_user_profile).transpose()
    }

    async fn save_block(&self, b: &MemoryBlock) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO agent_memory_block (
                id, created, updated, platformId, projectId, locationId, chatbotId,
                chatIdentityId, sessionKey, scope, label, value, sizeLimitTokens,
                importance, lastEditedBy, lastEditedAt, version
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                updated         = excluded.updated,
                platformId      = excluded.platformId,
                projectId       = excluded.projectId,
                locationId      = excluded.locationId,
                chatbotId       = excluded.chatbotId,
                chatIdentityId  = excluded.chatIdentityId,
                sessionKey      = excluded.sessionKey,
                scope           = excluded.scope,
                label           = excluded.label,
                value           = excluded.value,
                sizeLimitTokens = excluded.sizeLimitTokens,
                importance      = excluded.importance,
                lastEditedBy    = excluded.lastEditedBy,
                lastEditedAt    = excluded.lastEditedAt,
                version         = excluded.version
            "#,
        )
        .bind(&b.id)
        .bind(iso(b.created))
        .bind(iso(Utc::now()))
        .bind(&b.platform_id)
        .bind(&b.project_id)
        .bind(&b.location_id)
        .bind(&b.chatbot_id)
        .bind(&b.chat_identity_id)
        .bind(&b.session_key)
        .bind(b.scope.as_str())
        .bind(&b.label)
        .bind(&b.value)
        .bind(b.size_limit_tokens)
        .bind(b.importance)
        .bind(&b.last_edited_by)
        .bind(b.last_edited_at.map(iso))
        .bind(b.version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_block(&self, id: &str) -> Result<Option<MemoryBlock>, StorageError> {
        let row = sqlx::query("SELECT * FROM agent_memory_block WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_block).transpose()
    }

    async fn query_blocks(&self, q: &BlockFilter) -> Result<Vec<MemoryBlock>, StorageError> {
        let mut sql = String::from("SELECT * FROM agent_memory_block WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = &q.platform_id {
            sql.push_str(" AND platformId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            sql.push_str(" AND projectId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.chatbot_id {
            sql.push_str(" AND chatbotId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.chat_identity_id {
            sql.push_str(" AND chatIdentityId = ?");
            binds.push(s.clone());
        }
        if let Some(scope) = q.scope {
            sql.push_str(" AND scope = ?");
            binds.push(scope.as_str().to_string());
        }
        if let Some(s) = &q.label {
            sql.push_str(" AND label = ?");
            binds.push(s.clone());
        }
        sql.push_str(" ORDER BY updated DESC");
        if let Some(limit) = q.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = q.offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }
        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_block).collect()
    }

    async fn save_entity(&self, e: &MemoryEntity) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO memory_entity (
                id, created, updated, platformId, projectId, locationId, chatbotId,
                chatIdentityId, scope, type, canonicalName, aliases, description,
                metadata, validFrom, validTo, supersededById
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                updated        = excluded.updated,
                platformId     = excluded.platformId,
                projectId      = excluded.projectId,
                locationId     = excluded.locationId,
                chatbotId      = excluded.chatbotId,
                chatIdentityId = excluded.chatIdentityId,
                scope          = excluded.scope,
                type           = excluded.type,
                canonicalName  = excluded.canonicalName,
                aliases        = excluded.aliases,
                description    = excluded.description,
                metadata       = excluded.metadata,
                validFrom      = excluded.validFrom,
                validTo        = excluded.validTo,
                supersededById = excluded.supersededById
            "#,
        )
        .bind(&e.id)
        .bind(iso(e.created))
        .bind(iso(Utc::now()))
        .bind(&e.platform_id)
        .bind(&e.project_id)
        .bind(&e.location_id)
        .bind(&e.chatbot_id)
        .bind(&e.chat_identity_id)
        .bind(e.scope.as_str())
        .bind(&e.type_)
        .bind(&e.canonical_name)
        .bind(serde_json::to_string(&e.aliases).unwrap_or_else(|_| "[]".into()))
        .bind(&e.description)
        .bind(json_or_null(&e.metadata))
        .bind(iso(e.valid_from))
        .bind(e.valid_to.map(iso))
        .bind(&e.superseded_by_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_entity(&self, id: &str) -> Result<Option<MemoryEntity>, StorageError> {
        let row = sqlx::query("SELECT * FROM memory_entity WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_entity).transpose()
    }

    async fn query_entities(&self, q: &EntityFilter) -> Result<Vec<MemoryEntity>, StorageError> {
        let mut sql = String::from("SELECT * FROM memory_entity WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = &q.platform_id {
            sql.push_str(" AND platformId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            sql.push_str(" AND projectId = ?");
            binds.push(s.clone());
        }
        if let Some(scope) = q.scope {
            sql.push_str(" AND scope = ?");
            binds.push(scope.as_str().to_string());
        }
        if let Some(s) = &q.type_ {
            sql.push_str(" AND type = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.canonical_name {
            sql.push_str(" AND canonicalName = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.alias {
            // Match the alias against either the canonical name OR any
            // entry inside the `aliases` JSON array. SQLite's json_each
            // unpacks the array inline so the EXISTS subquery checks
            // membership without materializing a full join.
            sql.push_str(
                " AND (canonicalName = ? OR EXISTS (SELECT 1 FROM json_each(memory_entity.aliases) WHERE json_each.value = ?))",
            );
            binds.push(s.clone());
            binds.push(s.clone());
        }
        if q.current_only {
            sql.push_str(" AND validTo IS NULL");
        }
        sql.push_str(" ORDER BY updated DESC");
        if let Some(limit) = q.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = q.offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }
        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_entity).collect()
    }

    async fn save_edge(&self, e: &MemoryEdge) -> Result<(), StorageError> {
        // Pre-validate the XOR — the DB enforces this too, but failing
        // early lets us return a clear error instead of a CHECK violation.
        let has_id = e.object_id.is_some();
        let has_lit = e.object_literal.is_some();
        if has_id == has_lit {
            return Err(StorageError::Conflict(
                "memory_edge requires exactly one of object_id or object_literal".into(),
            ));
        }
        sqlx::query(
            r#"
            INSERT INTO memory_edge (
                id, created, updated, platformId, projectId, locationId, chatbotId,
                chatIdentityId, scope, subjectId, predicate, objectId, objectLiteral,
                weight, sourceMemoryId, metadata, validFrom, validTo
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                updated        = excluded.updated,
                platformId     = excluded.platformId,
                projectId      = excluded.projectId,
                locationId     = excluded.locationId,
                chatbotId      = excluded.chatbotId,
                chatIdentityId = excluded.chatIdentityId,
                scope          = excluded.scope,
                subjectId      = excluded.subjectId,
                predicate      = excluded.predicate,
                objectId       = excluded.objectId,
                objectLiteral  = excluded.objectLiteral,
                weight         = excluded.weight,
                sourceMemoryId = excluded.sourceMemoryId,
                metadata       = excluded.metadata,
                validFrom      = excluded.validFrom,
                validTo        = excluded.validTo
            "#,
        )
        .bind(&e.id)
        .bind(iso(e.created))
        .bind(iso(Utc::now()))
        .bind(&e.platform_id)
        .bind(&e.project_id)
        .bind(&e.location_id)
        .bind(&e.chatbot_id)
        .bind(&e.chat_identity_id)
        .bind(e.scope.as_str())
        .bind(&e.subject_id)
        .bind(&e.predicate)
        .bind(&e.object_id)
        .bind(&e.object_literal)
        .bind(e.weight)
        .bind(&e.source_memory_id)
        .bind(json_or_null(&e.metadata))
        .bind(iso(e.valid_from))
        .bind(e.valid_to.map(iso))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_edge(&self, id: &str) -> Result<Option<MemoryEdge>, StorageError> {
        let row = sqlx::query("SELECT * FROM memory_edge WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_edge).transpose()
    }

    async fn query_edges(&self, q: &EdgeFilter) -> Result<Vec<MemoryEdge>, StorageError> {
        let mut sql = String::from("SELECT * FROM memory_edge WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(s) = &q.platform_id {
            sql.push_str(" AND platformId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            sql.push_str(" AND projectId = ?");
            binds.push(s.clone());
        }
        if let Some(scope) = q.scope {
            sql.push_str(" AND scope = ?");
            binds.push(scope.as_str().to_string());
        }
        if let Some(s) = &q.subject_id {
            sql.push_str(" AND subjectId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.object_id {
            sql.push_str(" AND objectId = ?");
            binds.push(s.clone());
        }
        if let Some(s) = &q.predicate {
            sql.push_str(" AND predicate = ?");
            binds.push(s.clone());
        }
        // Point-in-time takes precedence over current_only: an edge was valid
        // AT `as_of` iff it had started and had not yet been closed.
        if let Some(at) = q.as_of {
            let at = iso(at);
            sql.push_str(" AND validFrom <= ? AND (validTo IS NULL OR validTo > ?)");
            binds.push(at.clone());
            binds.push(at);
        } else if q.current_only {
            sql.push_str(" AND validTo IS NULL");
        }
        sql.push_str(" ORDER BY updated DESC");
        if let Some(limit) = q.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = q.offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }
        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_edge).collect()
    }

    async fn invalidate_edge(&self, id: &str, at: DateTime<Utc>) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE memory_edge SET validTo = ?, updated = ? WHERE id = ? AND validTo IS NULL",
        )
        .bind(iso(at))
        .bind(iso(Utc::now()))
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_binding(&self, binding: &ProjectBinding) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO project_bindings (cwd, platform_id, project_id, created, updated)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(cwd) DO UPDATE SET
                platform_id = excluded.platform_id,
                project_id  = excluded.project_id,
                updated     = excluded.updated
            "#,
        )
        .bind(&binding.cwd)
        .bind(&binding.platform_id)
        .bind(&binding.project_id)
        .bind(binding.created.to_rfc3339())
        .bind(binding.updated.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_binding(&self, cwd: &str) -> Result<Option<ProjectBinding>, StorageError> {
        let row = sqlx::query(
            r#"
            SELECT cwd, platform_id, project_id, created, updated
            FROM project_bindings
            WHERE cwd = ?
            "#,
        )
        .bind(cwd)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_binding).transpose()
    }

    async fn list_bindings(&self) -> Result<Vec<ProjectBinding>, StorageError> {
        let rows = sqlx::query(
            r#"
            SELECT cwd, platform_id, project_id, created, updated
            FROM project_bindings
            ORDER BY updated DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_binding).collect()
    }

    async fn remove_binding(&self, cwd: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM project_bindings WHERE cwd = ?")
            .bind(cwd)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn get_sync_state(&self, key: &str) -> Result<Option<String>, StorageError> {
        let value: Option<String> =
            sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(value)
    }

    async fn set_sync_state(&self, key: &str, value: &str) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO sync_state (key, value, updated)
            VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            ON CONFLICT(key) DO UPDATE SET
                value   = excluded.value,
                updated = excluded.updated
            "#,
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn count_items(&self) -> Result<i64, StorageError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clawdbot_memory_item")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    async fn list_pending_for_push(&self, limit: u32) -> Result<Vec<MemoryItem>, StorageError> {
        let rows = sqlx::query(
            r#"
            SELECT
                id, created, updated,
                platformId, projectId, locationId, chatbotId, chatIdentityId, sessionKey,
                type, content, category, importance, source, metadata,
                scope, status, confidence, impact,
                supersededById, confirmedByUserId, confirmedAt, negativeRatingCount,
                validFrom, validTo, learnedAt, lastAccessedAt
            FROM clawdbot_memory_item
            WHERE sync_status = 'pending'
            ORDER BY updated ASC, id ASC
            LIMIT ?
            "#,
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_memory_item).collect()
    }

    async fn set_sync_status(&self, ids: &[String], status: &str) -> Result<(), StorageError> {
        if ids.is_empty() {
            return Ok(());
        }
        // SQLite can't bind a slice as a single parameter; build the
        // placeholders manually. Safe — `status` is bound separately and
        // ids only flow in from server responses we just sent.
        let placeholders = (0..ids.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let sql =
            format!("UPDATE clawdbot_memory_item SET sync_status = ? WHERE id IN ({placeholders})");
        let mut q = sqlx::query(&sql).bind(status);
        for id in ids {
            q = q.bind(id);
        }
        q.execute(&self.pool).await?;
        Ok(())
    }

    async fn set_embedding(&self, id: &str, vector: &[f32]) -> Result<(), StorageError> {
        // Write only the embedding column; deliberately do NOT bump `updated`,
        // so a re-embed never masquerades as a content change to the sync loop.
        let blob: Option<Vec<u8>> = if vector.is_empty() {
            None
        } else {
            Some(vec_to_blob(vector))
        };
        sqlx::query("UPDATE clawdbot_memory_item SET embedding = ? WHERE id = ?")
            .bind(blob)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn items_missing_embedding(
        &self,
        filter: &MemoryFilter,
        limit: u32,
    ) -> Result<Vec<MemoryItem>, StorageError> {
        let (clause, binds) = sqlite_filter_clause(filter);
        let sql = format!(
            "SELECT * FROM clawdbot_memory_item WHERE embedding IS NULL{clause} \
             ORDER BY learnedAt DESC LIMIT {limit}"
        );
        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_memory_item).collect()
    }

    async fn vector_search(
        &self,
        query: &[f32],
        filter: &MemoryFilter,
        limit: u32,
    ) -> Result<Vec<ScoredMemoryItem>, StorageError> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        // Brute-force cosine in Rust over the filtered candidate set. Fine at
        // local-dev volumes; the SaaS (postgres) backend uses the pgvector
        // HNSW index instead. The candidate scan is bounded so a large project
        // can't blow up memory — if a project ever outgrows this, that's the
        // signal to add `sqlite-vec` (the column already holds the vectors).
        const MAX_CANDIDATES: u32 = 50_000;
        let (clause, binds) = sqlite_filter_clause(filter);
        let sql = format!(
            "SELECT * FROM clawdbot_memory_item WHERE embedding IS NOT NULL{clause} \
             LIMIT {MAX_CANDIDATES}"
        );
        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;

        let mut scored: Vec<ScoredMemoryItem> = Vec::with_capacity(rows.len());
        for row in rows {
            let blob: Vec<u8> = row.try_get("embedding")?;
            let vec = blob_to_vec(&blob);
            // Skip rows whose stored vector has a different dimension than the
            // query (e.g. embedded under a previous model) — comparing them
            // would just inject noise.
            if vec.len() != query.len() {
                continue;
            }
            let similarity = memory_embed::cosine_similarity(query, &vec);
            let item = row_to_memory_item(row)?;
            scored.push(ScoredMemoryItem { item, similarity });
        }
        scored.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit as usize);
        Ok(scored)
    }

    async fn memory_stats(&self, filter: &MemoryFilter) -> Result<MemoryStats, StorageError> {
        let (clause, binds) = sqlite_filter_clause(filter);

        let total = sqlite_scalar_count(&self.pool, &clause, "", &binds).await?;
        let with_embedding =
            sqlite_scalar_count(&self.pool, &clause, " AND embedding IS NOT NULL", &binds).await?;
        let pattern_count = sqlite_scalar_count(
            &self.pool,
            &clause,
            " AND type = 'behavior_pattern'",
            &binds,
        )
        .await?;

        let by_scope = sqlite_grouped_counts(&self.pool, "scope", &clause, &binds).await?;
        let by_status = sqlite_grouped_counts(&self.pool, "status", &clause, &binds).await?;
        let by_type = sqlite_grouped_counts(&self.pool, "type", &clause, &binds).await?;

        Ok(MemoryStats {
            total,
            by_scope,
            by_status,
            by_type,
            with_embedding,
            without_embedding: (total - with_embedding).max(0),
            pattern_count,
        })
    }

    // ── typed attributes ────────────────────────────────────
    //
    // Desktop parity for the typed-attribute layer is deferred (the
    // billions-of-records path is server/Postgres-only). These stubs keep the
    // shared trait satisfied so the workspace compiles; calling them on the
    // SQLite backend is a clear, surfaced error rather than silent wrong data.

    async fn upsert_attribute_def(&self, _def: &AttributeDef) -> Result<(), StorageError> {
        Err(StorageError::Unsupported(
            "typed-attribute registry is Postgres-only in this increment".into(),
        ))
    }

    async fn query_attribute_defs(
        &self,
        _filter: &AttributeDefFilter,
    ) -> Result<Vec<AttributeDef>, StorageError> {
        Err(StorageError::Unsupported(
            "typed-attribute registry is Postgres-only in this increment".into(),
        ))
    }

    async fn save_typed_observations(
        &self,
        _observations: &[TypedObservation],
    ) -> Result<IngestReport, StorageError> {
        Err(StorageError::Unsupported(
            "typed observations are Postgres-only in this increment".into(),
        ))
    }

    async fn query_typed_observations(
        &self,
        _filter: &TypedObservationFilter,
    ) -> Result<Vec<TypedObservation>, StorageError> {
        Err(StorageError::Unsupported(
            "typed observations are Postgres-only in this increment".into(),
        ))
    }

    async fn get_accumulator(
        &self,
        _key: &AccumulatorKey,
    ) -> Result<Option<Accumulator>, StorageError> {
        Err(StorageError::Unsupported(
            "accumulators are Postgres-only in this increment".into(),
        ))
    }

    async fn migrate(&self) -> Result<i64, StorageError> {
        MIGRATIONS
            .run(&self.pool)
            .await
            .map_err(|e| StorageError::Other(anyhow::anyhow!(e)))?;
        // The latest applied version is the highest numbered migration we
        // shipped. sqlx tracks this internally; we surface it as a single
        // integer for diagnostics / sync handshakes.
        let latest: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);
        Ok(latest)
    }

    fn backend(&self) -> &'static str {
        "sqlite"
    }
}

/// Build the `AND ...`-prefixed WHERE fragment (sqlite `?` placeholders) and
/// ordered bind values for a `MemoryFilter`. Shared by `query`,
/// `vector_search`, and `items_missing_embedding` so the structural predicates
/// stay identical across all three read paths.
fn sqlite_filter_clause(f: &MemoryFilter) -> (String, Vec<String>) {
    let mut sql = String::new();
    let mut binds: Vec<String> = Vec::new();
    if let Some(scope) = f.scope {
        sql.push_str(" AND scope = ?");
        binds.push(scope.as_str().to_string());
    }
    if let Some(s) = &f.platform_id {
        sql.push_str(" AND platformId = ?");
        binds.push(s.clone());
    }
    if let Some(s) = &f.project_id {
        sql.push_str(" AND projectId = ?");
        binds.push(s.clone());
    }
    if let Some(s) = &f.agent_id {
        sql.push_str(" AND chatbotId = ?");
        binds.push(s.clone());
    }
    if let Some(s) = &f.user_id {
        sql.push_str(" AND chatIdentityId = ?");
        binds.push(s.clone());
    }
    if let Some(s) = &f.session_id {
        sql.push_str(" AND sessionKey = ?");
        binds.push(s.clone());
    }
    if let Some(status) = f.status {
        sql.push_str(" AND status = ?");
        binds.push(status.as_str().to_string());
    }
    if let Some(s) = &f.kind {
        sql.push_str(" AND type = ?");
        binds.push(s.clone());
    }
    if let Some(s) = &f.text_match {
        sql.push_str(" AND content LIKE ?");
        binds.push(format!("%{s}%"));
    }
    (sql, binds)
}

/// `SELECT COUNT(*)` with the filter `clause` plus an optional fixed `suffix`
/// predicate (never user input). The filter `binds` carry the parameters.
async fn sqlite_scalar_count(
    pool: &SqlitePool,
    clause: &str,
    suffix: &str,
    binds: &[String],
) -> Result<i64, StorageError> {
    let sql = format!("SELECT COUNT(*) FROM clawdbot_memory_item WHERE 1=1{clause}{suffix}");
    let mut q = sqlx::query_scalar::<_, i64>(&sql);
    for b in binds {
        q = q.bind(b);
    }
    Ok(q.fetch_one(pool).await?)
}

/// `SELECT <col>, COUNT(*) ... GROUP BY <col>` into a map. `col` is a fixed
/// identifier supplied by call sites (never user input); the filter `clause`
/// carries the parameterized predicates.
async fn sqlite_grouped_counts(
    pool: &SqlitePool,
    col: &str,
    clause: &str,
    binds: &[String],
) -> Result<BTreeMap<String, i64>, StorageError> {
    let sql = format!(
        "SELECT {col} AS k, COUNT(*) AS c FROM clawdbot_memory_item WHERE 1=1{clause} GROUP BY {col}"
    );
    let mut q = sqlx::query(&sql);
    for b in binds {
        q = q.bind(b);
    }
    let rows = q.fetch_all(pool).await?;
    let mut out = BTreeMap::new();
    for row in rows {
        let k: String = row.try_get("k")?;
        let c: i64 = row.try_get("c")?;
        out.insert(k, c);
    }
    Ok(out)
}

/// Pack an f32 vector into little-endian bytes for the `embedding` BLOB column.
fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Inverse of [`vec_to_blob`]. Trailing bytes that don't form a whole f32 are
/// dropped (defensive — a well-formed BLOB is always a multiple of 4).
fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn row_to_binding(row: sqlx::sqlite::SqliteRow) -> Result<ProjectBinding, StorageError> {
    let created: String = row.try_get("created")?;
    let updated: String = row.try_get("updated")?;
    Ok(ProjectBinding {
        cwd: row.try_get("cwd")?,
        platform_id: row.try_get("platform_id")?,
        project_id: row.try_get("project_id")?,
        created: parse_ts(&created)?,
        updated: parse_ts(&updated)?,
    })
}

// ────────────────────────────────────────────────────────────
// row → MemoryItem decoder
// ────────────────────────────────────────────────────────────

fn row_to_memory_item(row: sqlx::sqlite::SqliteRow) -> Result<MemoryItem, StorageError> {
    let scope: String = row.try_get("scope")?;
    let status: String = row.try_get("status")?;
    let impact: Option<String> = row.try_get("impact")?;
    let metadata_raw: Option<String> = row.try_get("metadata")?;
    let metadata = metadata_raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);

    Ok(MemoryItem {
        id: row.try_get("id")?,
        created: parse_ts(row.try_get::<String, _>("created")?.as_str())?,
        updated: parse_ts(row.try_get::<String, _>("updated")?.as_str())?,
        platform_id: row.try_get("platformId")?,
        project_id: row.try_get("projectId")?,
        location_id: row.try_get("locationId")?,
        chatbot_id: row.try_get("chatbotId")?,
        chat_identity_id: row.try_get("chatIdentityId")?,
        session_key: row.try_get("sessionKey")?,
        type_: row.try_get("type")?,
        content: row.try_get("content")?,
        category: row.try_get("category")?,
        importance: row.try_get::<f64, _>("importance")? as f32,
        source: row.try_get("source")?,
        metadata,
        scope: parse_scope(&scope)?,
        status: parse_status(&status)?,
        confidence: row.try_get::<f64, _>("confidence")? as f32,
        impact: impact.as_deref().map(parse_impact).transpose()?,
        superseded_by_id: row.try_get("supersededById")?,
        confirmed_by_user_id: row.try_get("confirmedByUserId")?,
        confirmed_at: row
            .try_get::<Option<String>, _>("confirmedAt")?
            .as_deref()
            .map(parse_ts)
            .transpose()?,
        negative_rating_count: row.try_get("negativeRatingCount")?,
        valid_from: parse_ts(row.try_get::<String, _>("validFrom")?.as_str())?,
        valid_to: row
            .try_get::<Option<String>, _>("validTo")?
            .as_deref()
            .map(parse_ts)
            .transpose()?,
        learned_at: parse_ts(row.try_get::<String, _>("learnedAt")?.as_str())?,
        last_accessed_at: parse_ts(row.try_get::<String, _>("lastAccessedAt")?.as_str())?,
    })
}

fn row_to_audit_event(row: sqlx::sqlite::SqliteRow) -> Result<MemoryAuditEvent, StorageError> {
    let metadata_raw: Option<String> = row.try_get("metadata")?;
    let metadata = metadata_raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);
    Ok(MemoryAuditEvent {
        id: row.try_get("id")?,
        created: parse_ts(row.try_get::<String, _>("created")?.as_str())?,
        platform_id: row.try_get("platformId")?,
        project_id: row.try_get("projectId")?,
        actor: row.try_get("actor")?,
        event_type: row.try_get("eventType")?,
        query: row.try_get("query")?,
        scope: row.try_get("scope")?,
        memory_ids: row.try_get("memoryIds")?,
        result_count: row.try_get("resultCount")?,
        metadata,
    })
}

fn row_to_feedback(row: sqlx::sqlite::SqliteRow) -> Result<MemoryFeedback, StorageError> {
    let rating_raw: String = row.try_get("rating")?;
    let rating = match rating_raw.as_str() {
        "positive" => FeedbackKind::Positive,
        "negative" => FeedbackKind::Negative,
        other => {
            return Err(StorageError::SchemaMismatch(format!(
                "unknown rating {other:?}"
            )))
        }
    };
    Ok(MemoryFeedback {
        id: row.try_get("id")?,
        created: parse_ts(row.try_get::<String, _>("created")?.as_str())?,
        updated: parse_ts(row.try_get::<String, _>("updated")?.as_str())?,
        memory_id: row.try_get("memoryId")?,
        response_id: row.try_get("responseId")?,
        rating,
        comment: row.try_get("comment")?,
        created_by_user_id: row.try_get("createdByUserId")?,
    })
}

fn json_or_null(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

fn json_or_default(v: &serde_json::Value, default: &str) -> String {
    if v.is_null() {
        default.to_string()
    } else {
        serde_json::to_string(v).unwrap_or_else(|_| default.to_string())
    }
}

fn parse_json(raw: Option<String>) -> serde_json::Value {
    raw.as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn row_to_contact(row: sqlx::sqlite::SqliteRow) -> Result<Contact, StorageError> {
    Ok(Contact {
        id: row.try_get("id")?,
        created: parse_ts(row.try_get::<String, _>("created")?.as_str())?,
        updated: parse_ts(row.try_get::<String, _>("updated")?.as_str())?,
        chatbot_id: row.try_get("chatbotId")?,
        project_id: row.try_get("projectId")?,
        name: row.try_get("name")?,
        phone: row.try_get("phone")?,
        email: row.try_get("email")?,
        relationship: row.try_get("relationship")?,
        is_emergency: row.try_get::<i64, _>("isEmergency")? != 0,
        priority: row.try_get("priority")?,
        notes: row.try_get("notes")?,
        metadata: parse_json(row.try_get("metadata")?),
        external_ids: parse_json(row.try_get("externalIds")?),
        tags: parse_json(row.try_get("tags")?),
        segment: row.try_get("segment")?,
        lifetime_value: row.try_get("lifetimeValue")?,
        last_interaction_at: row
            .try_get::<Option<String>, _>("lastInteractionAt")?
            .as_deref()
            .map(parse_ts)
            .transpose()?,
        chat_identity_id: row.try_get("chatIdentityId")?,
    })
}

fn row_to_contact_event(row: sqlx::sqlite::SqliteRow) -> Result<ContactEvent, StorageError> {
    Ok(ContactEvent {
        id: row.try_get("id")?,
        created: parse_ts(row.try_get::<String, _>("created")?.as_str())?,
        updated: parse_ts(row.try_get::<String, _>("updated")?.as_str())?,
        contact_id: row.try_get("contactId")?,
        project_id: row.try_get("projectId")?,
        chatbot_id: row.try_get("chatbotId")?,
        event_type: row.try_get("eventType")?,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        event_data: parse_json(row.try_get("eventData")?),
        occurred_at: parse_ts(row.try_get::<String, _>("occurredAt")?.as_str())?,
        source: row.try_get("source")?,
        engagement_rule_id: row.try_get("engagementRuleId")?,
        engagement_execution_id: row.try_get("engagementExecutionId")?,
    })
}

fn row_to_block(row: sqlx::sqlite::SqliteRow) -> Result<MemoryBlock, StorageError> {
    let scope: String = row.try_get("scope")?;
    Ok(MemoryBlock {
        id: row.try_get("id")?,
        created: parse_ts(row.try_get::<String, _>("created")?.as_str())?,
        updated: parse_ts(row.try_get::<String, _>("updated")?.as_str())?,
        platform_id: row.try_get("platformId")?,
        project_id: row.try_get("projectId")?,
        location_id: row.try_get("locationId")?,
        chatbot_id: row.try_get("chatbotId")?,
        chat_identity_id: row.try_get("chatIdentityId")?,
        session_key: row.try_get("sessionKey")?,
        scope: parse_scope(&scope)?,
        label: row.try_get("label")?,
        value: row.try_get("value")?,
        size_limit_tokens: row.try_get("sizeLimitTokens")?,
        importance: row.try_get::<f64, _>("importance")? as f32,
        last_edited_by: row.try_get("lastEditedBy")?,
        last_edited_at: row
            .try_get::<Option<String>, _>("lastEditedAt")?
            .as_deref()
            .map(parse_ts)
            .transpose()?,
        version: row.try_get("version")?,
    })
}

fn row_to_entity(row: sqlx::sqlite::SqliteRow) -> Result<MemoryEntity, StorageError> {
    let scope: String = row.try_get("scope")?;
    let aliases_raw: String = row.try_get("aliases")?;
    let aliases: Vec<String> = serde_json::from_str(&aliases_raw).unwrap_or_default();
    Ok(MemoryEntity {
        id: row.try_get("id")?,
        created: parse_ts(row.try_get::<String, _>("created")?.as_str())?,
        updated: parse_ts(row.try_get::<String, _>("updated")?.as_str())?,
        platform_id: row.try_get("platformId")?,
        project_id: row.try_get("projectId")?,
        location_id: row.try_get("locationId")?,
        chatbot_id: row.try_get("chatbotId")?,
        chat_identity_id: row.try_get("chatIdentityId")?,
        scope: parse_scope(&scope)?,
        type_: row.try_get("type")?,
        canonical_name: row.try_get("canonicalName")?,
        aliases,
        description: row.try_get("description")?,
        metadata: parse_json(row.try_get("metadata")?),
        valid_from: parse_ts(row.try_get::<String, _>("validFrom")?.as_str())?,
        valid_to: row
            .try_get::<Option<String>, _>("validTo")?
            .as_deref()
            .map(parse_ts)
            .transpose()?,
        superseded_by_id: row.try_get("supersededById")?,
    })
}

fn row_to_edge(row: sqlx::sqlite::SqliteRow) -> Result<MemoryEdge, StorageError> {
    let scope: String = row.try_get("scope")?;
    Ok(MemoryEdge {
        id: row.try_get("id")?,
        created: parse_ts(row.try_get::<String, _>("created")?.as_str())?,
        updated: parse_ts(row.try_get::<String, _>("updated")?.as_str())?,
        platform_id: row.try_get("platformId")?,
        project_id: row.try_get("projectId")?,
        location_id: row.try_get("locationId")?,
        chatbot_id: row.try_get("chatbotId")?,
        chat_identity_id: row.try_get("chatIdentityId")?,
        scope: parse_scope(&scope)?,
        subject_id: row.try_get("subjectId")?,
        predicate: row.try_get("predicate")?,
        object_id: row.try_get("objectId")?,
        object_literal: row.try_get("objectLiteral")?,
        weight: row.try_get::<f64, _>("weight")? as f32,
        source_memory_id: row.try_get("sourceMemoryId")?,
        metadata: parse_json(row.try_get("metadata")?),
        valid_from: parse_ts(row.try_get::<String, _>("validFrom")?.as_str())?,
        valid_to: row
            .try_get::<Option<String>, _>("validTo")?
            .as_deref()
            .map(parse_ts)
            .transpose()?,
    })
}

fn row_to_user_profile(row: sqlx::sqlite::SqliteRow) -> Result<UserProfile, StorageError> {
    Ok(UserProfile {
        id: row.try_get("id")?,
        created: parse_ts(row.try_get::<String, _>("created")?.as_str())?,
        updated: parse_ts(row.try_get::<String, _>("updated")?.as_str())?,
        chat_identity_id: row.try_get("chatIdentityId")?,
        project_id: row.try_get("projectId")?,
        platform_id: row.try_get("platformId")?,
        profile: parse_json(row.try_get("profile")?),
        profile_version: row.try_get("profileVersion")?,
        last_updated_from_session_id: row.try_get("lastUpdatedFromSessionId")?,
    })
}

fn iso(ts: DateTime<Utc>) -> String {
    // Millisecond precision RFC3339 string, matching the schema default
    // (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) — chrono's SecondsFormat::Millis
    // emits `YYYY-MM-DDTHH:MM:SS.sssZ`.
    ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>, StorageError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| StorageError::SchemaMismatch(format!("invalid timestamp {raw:?}: {e}")))
}

fn parse_scope(s: &str) -> Result<MemoryScope, StorageError> {
    Ok(match s {
        "platform" => MemoryScope::Platform,
        "project" => MemoryScope::Project,
        "location" => MemoryScope::Location,
        "agent" => MemoryScope::Agent,
        "user" => MemoryScope::User,
        "session" => MemoryScope::Session,
        _ => return Err(StorageError::SchemaMismatch(format!("unknown scope {s:?}"))),
    })
}

fn parse_status(s: &str) -> Result<MemoryStatus, StorageError> {
    Ok(match s {
        "pending" => MemoryStatus::Pending,
        "confirmed" => MemoryStatus::Confirmed,
        "superseded" => MemoryStatus::Superseded,
        "rejected" => MemoryStatus::Rejected,
        _ => {
            return Err(StorageError::SchemaMismatch(format!(
                "unknown status {s:?}"
            )))
        }
    })
}

fn parse_impact(s: &str) -> Result<MemoryImpact, StorageError> {
    Ok(match s {
        "high" => MemoryImpact::High,
        "low" => MemoryImpact::Low,
        _ => {
            return Err(StorageError::SchemaMismatch(format!(
                "unknown impact {s:?}"
            )))
        }
    })
}

// ────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::MemoryScope;

    async fn store() -> SqliteStore {
        let s = SqliteStore::connect("sqlite::memory:").await.unwrap();
        s.migrate().await.unwrap();
        s
    }

    fn new_item(id: &str) -> MemoryItem {
        MemoryItem::new(
            id,
            "plat_test",
            "fact",
            "the customer prefers email",
            MemoryScope::Project,
        )
    }

    #[tokio::test]
    async fn save_then_get_roundtrip() {
        let s = store().await;
        let item = new_item("mem_1");
        s.save(&item).await.unwrap();

        let got = s.get("mem_1").await.unwrap().expect("should exist");
        assert_eq!(got.id, "mem_1");
        assert_eq!(got.content, item.content);
        assert_eq!(got.scope, MemoryScope::Project);
        assert_eq!(got.status, MemoryStatus::Confirmed);
        assert_eq!(got.platform_id, "plat_test");
    }

    #[tokio::test]
    async fn save_is_upsert() {
        let s = store().await;
        let mut item = new_item("mem_2");
        s.save(&item).await.unwrap();
        item.content = "updated content".into();
        s.save(&item).await.unwrap();

        let got = s.get("mem_2").await.unwrap().unwrap();
        assert_eq!(got.content, "updated content");
    }

    #[tokio::test]
    async fn query_filters_by_project_and_text() {
        let s = store().await;
        let mut a = new_item("mem_3");
        a.project_id = Some("proj_a".into());
        a.content = "alpha".into();
        let mut b = new_item("mem_4");
        b.project_id = Some("proj_b".into());
        b.content = "beta".into();
        s.save(&a).await.unwrap();
        s.save(&b).await.unwrap();

        let q = MemoryQuery {
            filter: crate::MemoryFilter {
                project_id: Some("proj_a".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let rows = s.query(&q).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "mem_3");

        let q = MemoryQuery {
            filter: crate::MemoryFilter {
                text_match: Some("beta".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let rows = s.query(&q).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "mem_4");
    }

    #[tokio::test]
    async fn supersede_marks_old_and_links_new() {
        let s = store().await;
        let old = new_item("mem_old");
        let mut new = new_item("mem_new");
        new.content = "newer fact".into();
        s.save(&old).await.unwrap();
        s.save(&new).await.unwrap();

        s.supersede("mem_old", "mem_new").await.unwrap();

        let got = s.get("mem_old").await.unwrap().unwrap();
        assert_eq!(got.status, MemoryStatus::Superseded);
        assert_eq!(got.superseded_by_id.as_deref(), Some("mem_new"));
    }

    #[tokio::test]
    async fn supersede_rejects_unknown_target() {
        let s = store().await;
        let item = new_item("mem_only");
        s.save(&item).await.unwrap();
        let err = s.supersede("mem_only", "nonexistent").await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }

    #[tokio::test]
    async fn touch_bumps_last_accessed() {
        let s = store().await;
        let mut item = new_item("mem_touch");
        item.last_accessed_at = "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        s.save(&item).await.unwrap();

        s.touch("mem_touch").await.unwrap();

        let got = s.get("mem_touch").await.unwrap().unwrap();
        assert!(got.last_accessed_at > item.last_accessed_at);
    }

    #[tokio::test]
    async fn vector_search_ranks_by_cosine_and_respects_filter() {
        let s = store().await;
        let mut a = new_item("vec_a");
        a.project_id = Some("p".into());
        let mut b = new_item("vec_b");
        b.project_id = Some("p".into());
        let mut c = new_item("vec_c");
        c.project_id = Some("other".into());
        s.save(&a).await.unwrap();
        s.save(&b).await.unwrap();
        s.save(&c).await.unwrap();

        s.set_embedding("vec_a", &[1.0, 0.0, 0.0]).await.unwrap();
        s.set_embedding("vec_b", &[0.0, 1.0, 0.0]).await.unwrap();
        // Same direction as `a` but in a different project — must be filtered out.
        s.set_embedding("vec_c", &[1.0, 0.0, 0.0]).await.unwrap();

        let filter = crate::MemoryFilter {
            project_id: Some("p".into()),
            ..Default::default()
        };
        let hits = s
            .vector_search(&[1.0, 0.0, 0.0], &filter, 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 2, "only project-p items with embeddings");
        assert_eq!(hits[0].item.id, "vec_a", "closest first");
        assert!(hits[0].similarity > hits[1].similarity);
        assert!((hits[0].similarity - 1.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn vector_search_empty_query_or_missing_embeddings_returns_empty() {
        let s = store().await;
        let item = new_item("vec_only");
        s.save(&item).await.unwrap();
        let f = crate::MemoryFilter::default();
        // No embedding set yet → not a candidate.
        assert!(s
            .vector_search(&[1.0, 0.0], &f, 5)
            .await
            .unwrap()
            .is_empty());
        // Empty query short-circuits even once an embedding exists.
        s.set_embedding("vec_only", &[1.0, 0.0]).await.unwrap();
        assert!(s.vector_search(&[], &f, 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn items_missing_embedding_lists_then_excludes_after_set() {
        let s = store().await;
        s.save(&new_item("miss_a")).await.unwrap();
        s.save(&new_item("miss_b")).await.unwrap();
        let f = crate::MemoryFilter::default();
        assert_eq!(s.items_missing_embedding(&f, 10).await.unwrap().len(), 2);
        s.set_embedding("miss_a", &[0.1, 0.2]).await.unwrap();
        let missing = s.items_missing_embedding(&f, 10).await.unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, "miss_b");
    }

    #[tokio::test]
    async fn set_embedding_does_not_bump_updated() {
        let s = store().await;
        s.save(&new_item("vec_upd")).await.unwrap();
        let before = s.get("vec_upd").await.unwrap().unwrap().updated;
        s.set_embedding("vec_upd", &[1.0, 2.0, 3.0]).await.unwrap();
        let after = s.get("vec_upd").await.unwrap().unwrap().updated;
        assert_eq!(
            before, after,
            "embedding write must not bump updated (no re-sync)"
        );
        // Round-trips back out via search.
        let f = crate::MemoryFilter::default();
        let hits = s.vector_search(&[1.0, 2.0, 3.0], &f, 1).await.unwrap();
        assert_eq!(hits[0].item.id, "vec_upd");
    }

    #[tokio::test]
    async fn memory_stats_aggregates_counts_and_embedding_coverage() {
        let s = store().await;
        let mut a = new_item("st_a"); // fact / confirmed
        a.project_id = Some("p".into());
        let mut b = new_item("st_b");
        b.project_id = Some("p".into());
        b.type_ = "event".into();
        let mut c = new_item("st_c");
        c.project_id = Some("p".into());
        c.type_ = "behavior_pattern".into();
        s.save(&a).await.unwrap();
        s.save(&b).await.unwrap();
        s.save(&c).await.unwrap();
        s.set_embedding("st_a", &[0.1, 0.2, 0.3]).await.unwrap();

        let filter = crate::MemoryFilter {
            project_id: Some("p".into()),
            ..Default::default()
        };
        let stats = s.memory_stats(&filter).await.unwrap();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.with_embedding, 1);
        assert_eq!(stats.without_embedding, 2);
        assert_eq!(stats.pattern_count, 1);
        assert_eq!(stats.by_type.get("fact").copied(), Some(1));
        assert_eq!(stats.by_type.get("event").copied(), Some(1));
        assert_eq!(stats.by_type.get("behavior_pattern").copied(), Some(1));
        assert_eq!(stats.by_status.get("confirmed").copied(), Some(3));
        assert_eq!(stats.by_scope.get("project").copied(), Some(3));
    }

    #[tokio::test]
    async fn migrate_is_idempotent() {
        let s = store().await;
        let v1 = s.migrate().await.unwrap();
        let v2 = s.migrate().await.unwrap();
        assert_eq!(v1, v2);
        assert!(v1 >= 1);
    }

    #[tokio::test]
    async fn audit_record_and_query() {
        let s = store().await;
        let now = Utc::now();
        let ev = MemoryAuditEvent {
            id: "audit_1".into(),
            created: now,
            platform_id: "plat_test".into(),
            project_id: Some("proj_a".into()),
            actor: "user:u1".into(),
            event_type: "read".into(),
            query: Some("hello".into()),
            scope: Some("project".into()),
            memory_ids: Some("mem_1,mem_2".into()),
            result_count: 2,
            metadata: serde_json::json!({ "source": "test" }),
        };
        s.record_audit(&ev).await.unwrap();

        let rows = s
            .query_audit(&crate::AuditFilter {
                platform_id: Some("plat_test".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].actor, "user:u1");
        assert_eq!(rows[0].result_count, 2);
        assert_eq!(rows[0].metadata["source"], "test");
    }

    #[tokio::test]
    async fn contact_save_and_query() {
        let s = store().await;
        let now = Utc::now();
        let c = Contact {
            id: "ctc_1".into(),
            created: now,
            updated: now,
            chatbot_id: "cb_1".into(),
            project_id: Some("proj_a".into()),
            name: "Alice".into(),
            phone: None,
            email: Some("alice@example.com".into()),
            relationship: None,
            is_emergency: false,
            priority: 0,
            notes: None,
            metadata: serde_json::json!({}),
            external_ids: serde_json::json!({}),
            tags: serde_json::json!(["vip"]),
            segment: Some("vip".into()),
            lifetime_value: Some(123.45),
            last_interaction_at: Some(now),
            chat_identity_id: Some("id_a".into()),
        };
        s.save_contact(&c).await.unwrap();

        let got = s.get_contact("ctc_1").await.unwrap().unwrap();
        assert_eq!(got.name, "Alice");
        assert_eq!(got.email.as_deref(), Some("alice@example.com"));
        assert_eq!(got.tags, serde_json::json!(["vip"]));

        let rows = s
            .query_contacts(&crate::ContactFilter {
                project_id: Some("proj_a".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn contact_event_time_window_query() {
        let s = store().await;
        // Need the parent contact for the FK constraint.
        let now = Utc::now();
        let c = Contact {
            id: "ctc_evt".into(),
            created: now,
            updated: now,
            chatbot_id: "cb_1".into(),
            project_id: Some("proj_a".into()),
            name: "Bob".into(),
            phone: None,
            email: None,
            relationship: None,
            is_emergency: false,
            priority: 0,
            notes: None,
            metadata: serde_json::Value::Null,
            external_ids: serde_json::Value::Null,
            tags: serde_json::json!([]),
            segment: None,
            lifetime_value: None,
            last_interaction_at: None,
            chat_identity_id: None,
        };
        s.save_contact(&c).await.unwrap();

        let t_early = "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let t_mid = "2026-03-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let t_late = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        for (id, occurred) in [
            ("ev_early", t_early),
            ("ev_mid", t_mid),
            ("ev_late", t_late),
        ] {
            let e = ContactEvent {
                id: id.into(),
                created: occurred,
                updated: occurred,
                contact_id: "ctc_evt".into(),
                project_id: "proj_a".into(),
                chatbot_id: Some("cb_1".into()),
                event_type: "purchase".into(),
                title: id.into(),
                description: None,
                event_data: serde_json::Value::Null,
                occurred_at: occurred,
                source: None,
                engagement_rule_id: None,
                engagement_execution_id: None,
            };
            s.save_contact_event(&e).await.unwrap();
        }

        // Window [t_mid, t_late) should include only ev_mid.
        let rows = s
            .query_contact_events(&crate::ContactEventFilter {
                contact_id: Some("ctc_evt".into()),
                since: Some(t_mid),
                until: Some(t_late),
                ..Default::default()
            })
            .await
            .unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["ev_mid"]);

        // No window — all three, newest first.
        let rows = s
            .query_contact_events(&crate::ContactEventFilter {
                contact_id: Some("ctc_evt".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, "ev_late");
        assert_eq!(rows[2].id, "ev_early");
    }

    #[tokio::test]
    async fn block_upsert_and_filter() {
        let s = store().await;
        let now = Utc::now();
        let b = MemoryBlock {
            id: "blk_1".into(),
            created: now,
            updated: now,
            platform_id: "plat_test".into(),
            project_id: Some("proj_a".into()),
            location_id: None,
            chatbot_id: None,
            chat_identity_id: None,
            session_key: None,
            scope: MemoryScope::Project,
            label: "active_okr".into(),
            value: "Ship memory engine v1".into(),
            size_limit_tokens: 1000,
            importance: 0.9,
            last_edited_by: None,
            last_edited_at: None,
            version: 1,
        };
        s.save_block(&b).await.unwrap();
        let got = s.get_block("blk_1").await.unwrap().unwrap();
        assert_eq!(got.label, "active_okr");
        assert!((got.importance - 0.9).abs() < 1e-5);

        let rows = s
            .query_blocks(&crate::BlockFilter {
                project_id: Some("proj_a".into()),
                label: Some("active_okr".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn entity_with_aliases_roundtrip() {
        let s = store().await;
        let now = Utc::now();
        let e = MemoryEntity {
            id: "ent_1".into(),
            created: now,
            updated: now,
            platform_id: "plat_test".into(),
            project_id: Some("proj_a".into()),
            location_id: None,
            chatbot_id: None,
            chat_identity_id: None,
            scope: MemoryScope::Project,
            type_: "person".into(),
            canonical_name: "Sarah Chen".into(),
            aliases: vec!["Sarah".into(), "S. Chen".into()],
            description: None,
            metadata: serde_json::Value::Null,
            valid_from: now,
            valid_to: None,
            superseded_by_id: None,
        };
        s.save_entity(&e).await.unwrap();
        let got = s.get_entity("ent_1").await.unwrap().unwrap();
        assert_eq!(got.aliases, vec!["Sarah".to_string(), "S. Chen".into()]);
    }

    #[tokio::test]
    async fn edge_rejects_both_object_id_and_literal() {
        let s = store().await;
        let now = Utc::now();
        let bad = MemoryEdge {
            id: "edge_bad".into(),
            created: now,
            updated: now,
            platform_id: "plat_test".into(),
            project_id: Some("proj_a".into()),
            location_id: None,
            chatbot_id: None,
            chat_identity_id: None,
            scope: MemoryScope::Project,
            subject_id: "ent_1".into(),
            predicate: "works_at".into(),
            object_id: Some("ent_2".into()),
            object_literal: Some("Acme".into()), // both set — invalid
            weight: 1.0,
            source_memory_id: None,
            metadata: serde_json::Value::Null,
            valid_from: now,
            valid_to: None,
        };
        let err = s.save_edge(&bad).await.unwrap_err();
        assert!(matches!(err, StorageError::Conflict(_)));
    }

    #[tokio::test]
    async fn edge_literal_object_works() {
        let s = store().await;
        let now = Utc::now();
        // Need a subject entity for the FK constraint.
        s.save_entity(&MemoryEntity {
            id: "ent_subj".into(),
            created: now,
            updated: now,
            platform_id: "plat_test".into(),
            project_id: Some("proj_a".into()),
            location_id: None,
            chatbot_id: None,
            chat_identity_id: None,
            scope: MemoryScope::Project,
            type_: "person".into(),
            canonical_name: "Alex".into(),
            aliases: vec![],
            description: None,
            metadata: serde_json::Value::Null,
            valid_from: now,
            valid_to: None,
            superseded_by_id: None,
        })
        .await
        .unwrap();
        let edge = MemoryEdge {
            id: "edge_lit".into(),
            created: now,
            updated: now,
            platform_id: "plat_test".into(),
            project_id: Some("proj_a".into()),
            location_id: None,
            chatbot_id: None,
            chat_identity_id: None,
            scope: MemoryScope::Project,
            subject_id: "ent_subj".into(),
            predicate: "favorite_color".into(),
            object_id: None,
            object_literal: Some("blue".into()),
            weight: 1.0,
            source_memory_id: None,
            metadata: serde_json::Value::Null,
            valid_from: now,
            valid_to: None,
        };
        s.save_edge(&edge).await.unwrap();
        let got = s.get_edge("edge_lit").await.unwrap().unwrap();
        assert_eq!(got.object_literal.as_deref(), Some("blue"));

        let rows = s
            .query_edges(&crate::EdgeFilter {
                subject_id: Some("ent_subj".into()),
                predicate: Some("favorite_color".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn user_profile_natural_key_lookup() {
        let s = store().await;
        let now = Utc::now();
        let p = UserProfile {
            id: "up_1".into(),
            created: now,
            updated: now,
            chat_identity_id: "ci_alice".into(),
            project_id: "proj_a".into(),
            platform_id: "plat_test".into(),
            profile: serde_json::json!({ "prefers": "email" }),
            profile_version: 1,
            last_updated_from_session_id: None,
        };
        s.save_user_profile(&p).await.unwrap();

        let by_id = s.get_user_profile("up_1").await.unwrap().unwrap();
        assert_eq!(by_id.profile["prefers"], "email");

        let by_nat = s
            .get_user_profile_by_identity("ci_alice", "proj_a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_nat.id, "up_1");

        let missing = s
            .get_user_profile_by_identity("ci_alice", "proj_other")
            .await
            .unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn feedback_negative_bumps_count() {
        let s = store().await;
        let item = new_item("mem_fb");
        s.save(&item).await.unwrap();

        let fb = MemoryFeedback {
            id: "fb_neg".into(),
            created: Utc::now(),
            updated: Utc::now(),
            memory_id: "mem_fb".into(),
            response_id: None,
            rating: FeedbackKind::Negative,
            comment: Some("not useful".into()),
            created_by_user_id: "user_1".into(),
        };
        s.record_feedback(&fb).await.unwrap();

        let got = s.get("mem_fb").await.unwrap().unwrap();
        assert_eq!(got.negative_rating_count, 1);

        // Positive doesn't bump the counter.
        let fb_pos = MemoryFeedback {
            id: "fb_pos".into(),
            created: Utc::now(),
            updated: Utc::now(),
            memory_id: "mem_fb".into(),
            response_id: None,
            rating: FeedbackKind::Positive,
            comment: None,
            created_by_user_id: "user_2".into(),
        };
        s.record_feedback(&fb_pos).await.unwrap();
        let got = s.get("mem_fb").await.unwrap().unwrap();
        assert_eq!(got.negative_rating_count, 1);

        let rows = s
            .query_feedback(&crate::FeedbackFilter {
                memory_id: Some("mem_fb".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
    }
}
