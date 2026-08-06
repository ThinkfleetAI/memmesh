// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! PostgresStore — SaaS backend. Reads / writes the same tables the
//! existing TypeScript activepieces API uses. Schema parity is enforced by
//! a shared migration set; `migrate()` is safe to run against a database
//! whose tables already exist (every CREATE is `IF NOT EXISTS`).
//!
//! Column quoting note: TypeORM produces camelCase column names which
//! Postgres folds to lowercase unless quoted. The migration intentionally
//! quotes every identifier (`"platformId"`, etc.) and every read/write
//! here does the same.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memory_core::{
    Accumulator, AttributeDef, Contact, ContactEvent, DataType, FeedbackKind, IngestReport,
    MemoryAuditEvent, MemoryBlock, MemoryEdge, MemoryEntity, MemoryFeedback, MemoryImpact,
    MemoryItem, MemoryScope, MemoryStatus, ObservationStatus, ProjectBinding, TypedObservation,
    UserProfile,
};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::types::Json;
use sqlx::{PgPool, Row};

use crate::query::{
    AccumulatorKey, AttributeDefFilter, AuditFilter, BlockFilter, ContactEventFilter,
    ContactFilter, EdgeFilter, EntityFilter, FeedbackFilter, MemoryFilter, MemoryQuery,
    MemoryStats, ScoredMemoryItem, TypedObservationFilter,
};
use crate::validate;
use crate::{Storage, StorageError};
use std::collections::BTreeMap;

static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    /// Connect to a Postgres database. URL form:
    /// `postgres://user:pass@host:5432/dbname`.
    ///
    /// The connection pool is intentionally modest (8 connections) — most
    /// memory-engine traffic is short queries; bigger pools cause more
    /// problems than they solve in a shared cluster.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let pool = PgPoolOptions::new().max_connections(8).connect(url).await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl Storage for PostgresStore {
    async fn save(&self, item: &MemoryItem) -> Result<(), StorageError> {
        let metadata = Json(&item.metadata);
        sqlx::query(
            r#"
            INSERT INTO clawdbot_memory_item (
                id, created, updated,
                "platformId", "projectId", "locationId", "chatbotId", "chatIdentityId", "sessionKey",
                type, content, category, importance, source, metadata,
                scope, status, confidence, impact,
                "supersededById", "confirmedByUserId", "confirmedAt", "negativeRatingCount",
                "validFrom", "validTo", "learnedAt", "lastAccessedAt"
            ) VALUES (
                $1, $2, $3,
                $4, $5, $6, $7, $8, $9,
                $10, $11, $12, $13, $14, $15,
                $16, $17, $18, $19,
                $20, $21, $22, $23,
                $24, $25, $26, $27
            )
            ON CONFLICT (id) DO UPDATE SET
                updated               = EXCLUDED.updated,
                "platformId"          = EXCLUDED."platformId",
                "projectId"           = EXCLUDED."projectId",
                "locationId"          = EXCLUDED."locationId",
                "chatbotId"           = EXCLUDED."chatbotId",
                "chatIdentityId"      = EXCLUDED."chatIdentityId",
                "sessionKey"          = EXCLUDED."sessionKey",
                type                  = EXCLUDED.type,
                content               = EXCLUDED.content,
                category              = EXCLUDED.category,
                importance            = EXCLUDED.importance,
                source                = EXCLUDED.source,
                metadata              = EXCLUDED.metadata,
                scope                 = EXCLUDED.scope,
                status                = EXCLUDED.status,
                confidence            = EXCLUDED.confidence,
                impact                = EXCLUDED.impact,
                "supersededById"      = EXCLUDED."supersededById",
                "confirmedByUserId"   = EXCLUDED."confirmedByUserId",
                "confirmedAt"         = EXCLUDED."confirmedAt",
                "negativeRatingCount" = EXCLUDED."negativeRatingCount",
                "validFrom"           = EXCLUDED."validFrom",
                "validTo"             = EXCLUDED."validTo",
                "learnedAt"           = EXCLUDED."learnedAt",
                "lastAccessedAt"      = EXCLUDED."lastAccessedAt"
            "#,
        )
        .bind(&item.id)
        .bind(item.created)
        .bind(Utc::now()) // updated bumped on every write
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
        .bind(metadata)
        .bind(item.scope.as_str())
        .bind(item.status.as_str())
        .bind(item.confidence)
        .bind(item.impact.map(|i| i.as_str()))
        .bind(&item.superseded_by_id)
        .bind(&item.confirmed_by_user_id)
        .bind(item.confirmed_at)
        .bind(item.negative_rating_count)
        .bind(item.valid_from)
        .bind(item.valid_to)
        .bind(item.learned_at)
        .bind(item.last_accessed_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryItem>, StorageError> {
        let row = sqlx::query("SELECT * FROM clawdbot_memory_item WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_memory_item).transpose()
    }

    async fn query(&self, q: &MemoryQuery) -> Result<Vec<MemoryItem>, StorageError> {
        // Build numbered placeholders ($1, $2, ...) as filters are appended.
        let mut sql = String::from("SELECT * FROM clawdbot_memory_item WHERE 1=1");
        let (clause, binds, _) = pg_filter_clause(&q.filter, 0);
        sql.push_str(&clause);

        sql.push_str(" ORDER BY \"learnedAt\" DESC");
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
        let exists: Option<(String,)> =
            sqlx::query_as("SELECT id FROM clawdbot_memory_item WHERE id = $1")
                .bind(by_id)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(StorageError::NotFound(by_id.to_string()));
        }
        let result = sqlx::query(
            "UPDATE clawdbot_memory_item
             SET status = 'superseded',
                 \"supersededById\" = $1,
                 updated = $2
             WHERE id = $3",
        )
        .bind(by_id)
        .bind(Utc::now())
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
        let result =
            sqlx::query("UPDATE clawdbot_memory_item SET \"lastAccessedAt\" = $1 WHERE id = $2")
                .bind(Utc::now())
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
            sqlx::query("DELETE FROM clawdbot_memory_item WHERE id = $1")
                .bind(id)
                .execute(&self.pool)
                .await?
        } else {
            sqlx::query(
                "UPDATE clawdbot_memory_item
                 SET status = 'rejected', updated = $1
                 WHERE id = $2",
            )
            .bind(Utc::now())
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
                id, created, "platformId", "projectId", actor, "eventType",
                query, scope, "memoryIds", "resultCount", metadata
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
        )
        .bind(&event.id)
        .bind(event.created)
        .bind(&event.platform_id)
        .bind(&event.project_id)
        .bind(&event.actor)
        .bind(&event.event_type)
        .bind(&event.query)
        .bind(&event.scope)
        .bind(&event.memory_ids)
        .bind(event.result_count)
        .bind(Json(&event.metadata))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn query_audit(&self, q: &AuditFilter) -> Result<Vec<MemoryAuditEvent>, StorageError> {
        let mut sql = String::from("SELECT * FROM memory_audit_event WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        let mut n = 0usize;

        if let Some(s) = &q.platform_id {
            n += 1;
            sql.push_str(&format!(" AND \"platformId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            n += 1;
            sql.push_str(&format!(" AND \"projectId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.actor {
            n += 1;
            sql.push_str(&format!(" AND actor = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.event_type {
            n += 1;
            sql.push_str(&format!(" AND \"eventType\" = ${n}"));
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
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            INSERT INTO agent_memory_feedback (
                id, created, updated, "memoryId", "responseId", rating,
                comment, "createdByUserId"
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(&fb.id)
        .bind(fb.created)
        .bind(Utc::now())
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
                r#"UPDATE clawdbot_memory_item
                   SET "negativeRatingCount" = "negativeRatingCount" + 1,
                       updated = $1
                   WHERE id = $2"#,
            )
            .bind(Utc::now())
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
        let mut n = 0usize;

        if let Some(s) = &q.memory_id {
            n += 1;
            sql.push_str(&format!(" AND \"memoryId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.created_by_user_id {
            n += 1;
            sql.push_str(&format!(" AND \"createdByUserId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(rating) = q.rating {
            n += 1;
            sql.push_str(&format!(" AND rating = ${n}"));
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
                id, created, updated, "chatbotId", "projectId", name, phone, email,
                relationship, "isEmergency", priority, notes, metadata, "externalIds",
                tags, segment, "lifetimeValue", "lastInteractionAt", "chatIdentityId"
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8,
                $9, $10, $11, $12, $13, $14,
                $15, $16, $17, $18, $19
            )
            ON CONFLICT (id) DO UPDATE SET
                updated             = EXCLUDED.updated,
                "chatbotId"         = EXCLUDED."chatbotId",
                "projectId"         = EXCLUDED."projectId",
                name                = EXCLUDED.name,
                phone               = EXCLUDED.phone,
                email               = EXCLUDED.email,
                relationship        = EXCLUDED.relationship,
                "isEmergency"       = EXCLUDED."isEmergency",
                priority            = EXCLUDED.priority,
                notes               = EXCLUDED.notes,
                metadata            = EXCLUDED.metadata,
                "externalIds"       = EXCLUDED."externalIds",
                tags                = EXCLUDED.tags,
                segment             = EXCLUDED.segment,
                "lifetimeValue"     = EXCLUDED."lifetimeValue",
                "lastInteractionAt" = EXCLUDED."lastInteractionAt",
                "chatIdentityId"    = EXCLUDED."chatIdentityId"
            "#,
        )
        .bind(&c.id)
        .bind(c.created)
        .bind(Utc::now())
        .bind(&c.chatbot_id)
        .bind(&c.project_id)
        .bind(&c.name)
        .bind(&c.phone)
        .bind(&c.email)
        .bind(&c.relationship)
        .bind(c.is_emergency)
        .bind(c.priority)
        .bind(&c.notes)
        .bind(Json(&c.metadata))
        .bind(Json(&c.external_ids))
        .bind(Json(&c.tags))
        .bind(&c.segment)
        .bind(c.lifetime_value)
        .bind(c.last_interaction_at)
        .bind(&c.chat_identity_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_contact(&self, id: &str) -> Result<Option<Contact>, StorageError> {
        let row = sqlx::query("SELECT * FROM clawdbot_contact WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_contact).transpose()
    }

    async fn query_contacts(&self, q: &ContactFilter) -> Result<Vec<Contact>, StorageError> {
        let mut sql = String::from("SELECT * FROM clawdbot_contact WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        let mut n = 0usize;

        if let Some(s) = &q.chatbot_id {
            n += 1;
            sql.push_str(&format!(" AND \"chatbotId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            n += 1;
            sql.push_str(&format!(" AND \"projectId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.chat_identity_id {
            n += 1;
            sql.push_str(&format!(" AND \"chatIdentityId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.email {
            n += 1;
            sql.push_str(&format!(" AND email = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.segment {
            n += 1;
            sql.push_str(&format!(" AND segment = ${n}"));
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
                id, created, updated, "contactId", "projectId", "chatbotId",
                "eventType", title, description, "eventData", "occurredAt", source,
                "engagementRuleId", "engagementExecutionId"
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11, $12,
                $13, $14
            )
            ON CONFLICT (id) DO UPDATE SET
                updated                  = EXCLUDED.updated,
                "contactId"              = EXCLUDED."contactId",
                "projectId"              = EXCLUDED."projectId",
                "chatbotId"              = EXCLUDED."chatbotId",
                "eventType"              = EXCLUDED."eventType",
                title                    = EXCLUDED.title,
                description              = EXCLUDED.description,
                "eventData"              = EXCLUDED."eventData",
                "occurredAt"             = EXCLUDED."occurredAt",
                source                   = EXCLUDED.source,
                "engagementRuleId"       = EXCLUDED."engagementRuleId",
                "engagementExecutionId"  = EXCLUDED."engagementExecutionId"
            "#,
        )
        .bind(&e.id)
        .bind(e.created)
        .bind(Utc::now())
        .bind(&e.contact_id)
        .bind(&e.project_id)
        .bind(&e.chatbot_id)
        .bind(&e.event_type)
        .bind(&e.title)
        .bind(&e.description)
        .bind(Json(&e.event_data))
        .bind(e.occurred_at)
        .bind(&e.source)
        .bind(&e.engagement_rule_id)
        .bind(&e.engagement_execution_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_contact_event(&self, id: &str) -> Result<Option<ContactEvent>, StorageError> {
        let row = sqlx::query("SELECT * FROM clawdbot_contact_event WHERE id = $1")
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
        let mut string_binds: Vec<String> = Vec::new();
        let mut ts_binds: Vec<DateTime<Utc>> = Vec::new();
        let mut n = 0usize;

        // We split bind types because sqlx::query.bind takes typed values;
        // the placeholder string is what matters for parameter ordering.
        if let Some(s) = &q.contact_id {
            n += 1;
            sql.push_str(&format!(" AND \"contactId\" = ${n}"));
            string_binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            n += 1;
            sql.push_str(&format!(" AND \"projectId\" = ${n}"));
            string_binds.push(s.clone());
        }
        if let Some(s) = &q.event_type {
            n += 1;
            sql.push_str(&format!(" AND \"eventType\" = ${n}"));
            string_binds.push(s.clone());
        }
        if let Some(ts) = q.since {
            n += 1;
            sql.push_str(&format!(" AND \"occurredAt\" >= ${n}"));
            ts_binds.push(ts);
        }
        if let Some(ts) = q.until {
            n += 1;
            sql.push_str(&format!(" AND \"occurredAt\" < ${n}"));
            ts_binds.push(ts);
        }
        sql.push_str(" ORDER BY \"occurredAt\" DESC");
        if let Some(limit) = q.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = q.offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }

        let mut qb = sqlx::query(&sql);
        for b in &string_binds {
            qb = qb.bind(b);
        }
        for b in &ts_binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_contact_event).collect()
    }

    async fn count_contact_events_since(
        &self,
        project_id: &str,
        since: DateTime<Utc>,
    ) -> Result<i64, StorageError> {
        // Per-tenant monthly events cap. Scoped by "projectId" — the
        // events table has no "platformId" column, so this is the
        // finest tenant grain available. "occurredAt" is the event
        // timestamp; `since` is start-of-month (UTC).
        let count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM clawdbot_contact_event
             WHERE "projectId" = $1 AND "occurredAt" >= $2
            "#,
        )
        .bind(project_id)
        .bind(since)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    async fn save_user_profile(&self, p: &UserProfile) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO agent_user_profile (
                id, created, updated, "chatIdentityId", "projectId", "platformId",
                profile, "profileVersion", "lastUpdatedFromSessionId"
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (id) DO UPDATE SET
                updated                    = EXCLUDED.updated,
                "chatIdentityId"           = EXCLUDED."chatIdentityId",
                "projectId"                = EXCLUDED."projectId",
                "platformId"               = EXCLUDED."platformId",
                profile                    = EXCLUDED.profile,
                "profileVersion"           = EXCLUDED."profileVersion",
                "lastUpdatedFromSessionId" = EXCLUDED."lastUpdatedFromSessionId"
            "#,
        )
        .bind(&p.id)
        .bind(p.created)
        .bind(Utc::now())
        .bind(&p.chat_identity_id)
        .bind(&p.project_id)
        .bind(&p.platform_id)
        .bind(Json(&p.profile))
        .bind(p.profile_version)
        .bind(&p.last_updated_from_session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_user_profile(&self, id: &str) -> Result<Option<UserProfile>, StorageError> {
        let row = sqlx::query("SELECT * FROM agent_user_profile WHERE id = $1")
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
            r#"SELECT * FROM agent_user_profile
               WHERE "chatIdentityId" = $1 AND "projectId" = $2"#,
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
                id, created, updated, "platformId", "projectId", "locationId",
                "chatbotId", "chatIdentityId", "sessionKey", scope, label, value,
                "sizeLimitTokens", importance, "lastEditedBy", "lastEditedAt", version
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11, $12,
                $13, $14, $15, $16, $17
            )
            ON CONFLICT (id) DO UPDATE SET
                updated           = EXCLUDED.updated,
                "platformId"      = EXCLUDED."platformId",
                "projectId"       = EXCLUDED."projectId",
                "locationId"      = EXCLUDED."locationId",
                "chatbotId"       = EXCLUDED."chatbotId",
                "chatIdentityId"  = EXCLUDED."chatIdentityId",
                "sessionKey"      = EXCLUDED."sessionKey",
                scope             = EXCLUDED.scope,
                label             = EXCLUDED.label,
                value             = EXCLUDED.value,
                "sizeLimitTokens" = EXCLUDED."sizeLimitTokens",
                importance        = EXCLUDED.importance,
                "lastEditedBy"    = EXCLUDED."lastEditedBy",
                "lastEditedAt"    = EXCLUDED."lastEditedAt",
                version           = EXCLUDED.version
            "#,
        )
        .bind(&b.id)
        .bind(b.created)
        .bind(Utc::now())
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
        .bind(b.last_edited_at)
        .bind(b.version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_block(&self, id: &str) -> Result<Option<MemoryBlock>, StorageError> {
        let row = sqlx::query("SELECT * FROM agent_memory_block WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_block).transpose()
    }

    async fn query_blocks(&self, q: &BlockFilter) -> Result<Vec<MemoryBlock>, StorageError> {
        let mut sql = String::from("SELECT * FROM agent_memory_block WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        let mut n = 0usize;
        if let Some(s) = &q.platform_id {
            n += 1;
            sql.push_str(&format!(" AND \"platformId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            n += 1;
            sql.push_str(&format!(" AND \"projectId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.chatbot_id {
            n += 1;
            sql.push_str(&format!(" AND \"chatbotId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.chat_identity_id {
            n += 1;
            sql.push_str(&format!(" AND \"chatIdentityId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(scope) = q.scope {
            n += 1;
            sql.push_str(&format!(" AND scope = ${n}"));
            binds.push(scope.as_str().to_string());
        }
        if let Some(s) = &q.label {
            n += 1;
            sql.push_str(&format!(" AND label = ${n}"));
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
                id, created, updated, "platformId", "projectId", "locationId",
                "chatbotId", "chatIdentityId", scope, type, "canonicalName",
                aliases, description, metadata, "validFrom", "validTo", "supersededById"
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11,
                $12, $13, $14, $15, $16, $17
            )
            ON CONFLICT (id) DO UPDATE SET
                updated          = EXCLUDED.updated,
                "platformId"     = EXCLUDED."platformId",
                "projectId"      = EXCLUDED."projectId",
                "locationId"     = EXCLUDED."locationId",
                "chatbotId"      = EXCLUDED."chatbotId",
                "chatIdentityId" = EXCLUDED."chatIdentityId",
                scope            = EXCLUDED.scope,
                type             = EXCLUDED.type,
                "canonicalName"  = EXCLUDED."canonicalName",
                aliases          = EXCLUDED.aliases,
                description      = EXCLUDED.description,
                metadata         = EXCLUDED.metadata,
                "validFrom"      = EXCLUDED."validFrom",
                "validTo"        = EXCLUDED."validTo",
                "supersededById" = EXCLUDED."supersededById"
            "#,
        )
        .bind(&e.id)
        .bind(e.created)
        .bind(Utc::now())
        .bind(&e.platform_id)
        .bind(&e.project_id)
        .bind(&e.location_id)
        .bind(&e.chatbot_id)
        .bind(&e.chat_identity_id)
        .bind(e.scope.as_str())
        .bind(&e.type_)
        .bind(&e.canonical_name)
        .bind(&e.aliases) // Vec<String> -> text[]
        .bind(&e.description)
        .bind(Json(&e.metadata))
        .bind(e.valid_from)
        .bind(e.valid_to)
        .bind(&e.superseded_by_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_entity(&self, id: &str) -> Result<Option<MemoryEntity>, StorageError> {
        let row = sqlx::query("SELECT * FROM memory_entity WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_entity).transpose()
    }

    async fn query_entities(&self, q: &EntityFilter) -> Result<Vec<MemoryEntity>, StorageError> {
        let mut sql = String::from("SELECT * FROM memory_entity WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        let mut n = 0usize;
        if let Some(s) = &q.platform_id {
            n += 1;
            sql.push_str(&format!(" AND \"platformId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            n += 1;
            sql.push_str(&format!(" AND \"projectId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(scope) = q.scope {
            n += 1;
            sql.push_str(&format!(" AND scope = ${n}"));
            binds.push(scope.as_str().to_string());
        }
        if let Some(s) = &q.type_ {
            n += 1;
            sql.push_str(&format!(" AND type = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.canonical_name {
            n += 1;
            sql.push_str(&format!(" AND \"canonicalName\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.alias {
            // Postgres stores aliases as text[]; ANY() does the
            // membership check without unnest. The OR with
            // canonicalName lets one parameter find an entity by
            // either its registered name or any of its known aliases.
            n += 1;
            sql.push_str(&format!(
                " AND (\"canonicalName\" = ${n} OR ${n} = ANY(aliases))",
            ));
            binds.push(s.clone());
        }
        if q.current_only {
            sql.push_str(" AND \"validTo\" IS NULL");
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
                id, created, updated, "platformId", "projectId", "locationId",
                "chatbotId", "chatIdentityId", scope, "subjectId", predicate,
                "objectId", "objectLiteral", weight, "sourceMemoryId", metadata,
                "validFrom", "validTo"
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11,
                $12, $13, $14, $15, $16,
                $17, $18
            )
            ON CONFLICT (id) DO UPDATE SET
                updated          = EXCLUDED.updated,
                "platformId"     = EXCLUDED."platformId",
                "projectId"      = EXCLUDED."projectId",
                "locationId"     = EXCLUDED."locationId",
                "chatbotId"      = EXCLUDED."chatbotId",
                "chatIdentityId" = EXCLUDED."chatIdentityId",
                scope            = EXCLUDED.scope,
                "subjectId"      = EXCLUDED."subjectId",
                predicate        = EXCLUDED.predicate,
                "objectId"       = EXCLUDED."objectId",
                "objectLiteral"  = EXCLUDED."objectLiteral",
                weight           = EXCLUDED.weight,
                "sourceMemoryId" = EXCLUDED."sourceMemoryId",
                metadata         = EXCLUDED.metadata,
                "validFrom"      = EXCLUDED."validFrom",
                "validTo"        = EXCLUDED."validTo"
            "#,
        )
        .bind(&e.id)
        .bind(e.created)
        .bind(Utc::now())
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
        .bind(Json(&e.metadata))
        .bind(e.valid_from)
        .bind(e.valid_to)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_edge(&self, id: &str) -> Result<Option<MemoryEdge>, StorageError> {
        let row = sqlx::query("SELECT * FROM memory_edge WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_edge).transpose()
    }

    async fn query_edges(&self, q: &EdgeFilter) -> Result<Vec<MemoryEdge>, StorageError> {
        let mut sql = String::from("SELECT * FROM memory_edge WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        let mut n = 0usize;
        if let Some(s) = &q.platform_id {
            n += 1;
            sql.push_str(&format!(" AND \"platformId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.project_id {
            n += 1;
            sql.push_str(&format!(" AND \"projectId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(scope) = q.scope {
            n += 1;
            sql.push_str(&format!(" AND scope = ${n}"));
            binds.push(scope.as_str().to_string());
        }
        if let Some(s) = &q.subject_id {
            n += 1;
            sql.push_str(&format!(" AND \"subjectId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.object_id {
            n += 1;
            sql.push_str(&format!(" AND \"objectId\" = ${n}"));
            binds.push(s.clone());
        }
        if let Some(s) = &q.predicate {
            n += 1;
            sql.push_str(&format!(" AND predicate = ${n}"));
            binds.push(s.clone());
        }
        // Point-in-time takes precedence over current_only. Cast the text
        // binds to timestamptz so the comparison is temporal, not lexical.
        if let Some(at) = q.as_of {
            let at_s = at.to_rfc3339();
            n += 1;
            let a = n;
            binds.push(at_s.clone());
            n += 1;
            let b = n;
            binds.push(at_s);
            sql.push_str(&format!(
                " AND \"validFrom\" <= ${a}::timestamptz AND (\"validTo\" IS NULL OR \"validTo\" > ${b}::timestamptz)"
            ));
        } else if q.current_only {
            sql.push_str(" AND \"validTo\" IS NULL");
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
            r#"UPDATE memory_edge SET "validTo" = $1, updated = now() WHERE id = $2 AND "validTo" IS NULL"#,
        )
        .bind(at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_binding(&self, binding: &ProjectBinding) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO project_bindings (cwd, platform_id, project_id, created, updated)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (cwd) DO UPDATE SET
                platform_id = EXCLUDED.platform_id,
                project_id  = EXCLUDED.project_id,
                updated     = EXCLUDED.updated
            "#,
        )
        .bind(&binding.cwd)
        .bind(&binding.platform_id)
        .bind(&binding.project_id)
        .bind(binding.created)
        .bind(binding.updated)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_binding(&self, cwd: &str) -> Result<Option<ProjectBinding>, StorageError> {
        let row = sqlx::query(
            r#"
            SELECT cwd, platform_id, project_id, created, updated
            FROM project_bindings
            WHERE cwd = $1
            "#,
        )
        .bind(cwd)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_binding_pg).transpose()
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
        rows.into_iter().map(row_to_binding_pg).collect()
    }

    async fn remove_binding(&self, cwd: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM project_bindings WHERE cwd = $1")
            .bind(cwd)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn get_sync_state(&self, key: &str) -> Result<Option<String>, StorageError> {
        let value: Option<String> =
            sqlx::query_scalar("SELECT value FROM sync_state WHERE key = $1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(value)
    }

    async fn set_sync_state(&self, key: &str, value: &str) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO sync_state (key, value, updated)
            VALUES ($1, $2, NOW())
            ON CONFLICT (key) DO UPDATE SET
                value   = EXCLUDED.value,
                updated = EXCLUDED.updated
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

    async fn list_pending_for_push(&self, _limit: u32) -> Result<Vec<MemoryItem>, StorageError> {
        // Postgres backend is only used by the SaaS itself; it doesn't
        // push to anyone. Returning empty keeps the trait satisfied.
        Ok(Vec::new())
    }

    async fn set_sync_status(&self, _ids: &[String], _status: &str) -> Result<(), StorageError> {
        // See list_pending_for_push — no-op on the SaaS-side backend.
        Ok(())
    }

    async fn set_embedding(&self, id: &str, vector: &[f32]) -> Result<(), StorageError> {
        // Write only the embedding column; deliberately do NOT bump `updated`,
        // so a re-embed never looks like a content change to the sync loop.
        // The vector is bound as a text literal and cast to pgvector — keeps
        // the driver dependency-free.
        if vector.is_empty() {
            sqlx::query("UPDATE clawdbot_memory_item SET embedding = NULL WHERE id = $1")
                .bind(id)
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query("UPDATE clawdbot_memory_item SET embedding = $1::vector WHERE id = $2")
                .bind(pg_vector_literal(vector))
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    async fn items_missing_embedding(
        &self,
        filter: &MemoryFilter,
        limit: u32,
    ) -> Result<Vec<MemoryItem>, StorageError> {
        let (clause, binds, _) = pg_filter_clause(filter, 0);
        let sql = format!(
            "SELECT * FROM clawdbot_memory_item WHERE embedding IS NULL{clause} \
             ORDER BY \"learnedAt\" DESC LIMIT {limit}"
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
        // Native pgvector: HNSW index does the ANN, `<=>` is cosine distance,
        // so `1 - distance` is cosine similarity. `$1` holds the query vector
        // (text literal cast to ::vector); filter binds follow from $2.
        let (clause, binds, _) = pg_filter_clause(filter, 1);
        let sql = format!(
            "SELECT *, 1 - (embedding <=> $1::vector) AS similarity \
             FROM clawdbot_memory_item \
             WHERE embedding IS NOT NULL{clause} \
             ORDER BY embedding <=> $1::vector LIMIT {limit}"
        );
        let mut qb = sqlx::query(&sql).bind(pg_vector_literal(query));
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let similarity: f64 = row.try_get("similarity")?;
            let item = row_to_memory_item(row)?;
            out.push(ScoredMemoryItem {
                item,
                similarity: similarity as f32,
            });
        }
        Ok(out)
    }

    async fn memory_stats(&self, filter: &MemoryFilter) -> Result<MemoryStats, StorageError> {
        let (clause, binds, _) = pg_filter_clause(filter, 0);
        let scalar = |suffix: &str| {
            format!("SELECT COUNT(*) FROM clawdbot_memory_item WHERE 1=1{clause}{suffix}")
        };
        let run_scalar = |sql: String| {
            let binds = binds.clone();
            let pool = self.pool.clone();
            async move {
                let mut q = sqlx::query_scalar::<_, i64>(&sql);
                for b in &binds {
                    q = q.bind(b);
                }
                q.fetch_one(&pool).await
            }
        };

        let total = run_scalar(scalar("")).await?;
        let with_embedding = run_scalar(scalar(" AND embedding IS NOT NULL")).await?;
        let pattern_count = run_scalar(scalar(" AND type = 'behavior_pattern'")).await?;

        let by_scope = pg_grouped_counts(&self.pool, "scope", &clause, &binds).await?;
        let by_status = pg_grouped_counts(&self.pool, "status", &clause, &binds).await?;
        let by_type = pg_grouped_counts(&self.pool, "type", &clause, &binds).await?;

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

    async fn upsert_attribute_def(&self, def: &AttributeDef) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO memory_attribute_registry (
                id, created, updated,
                "platformId", "projectId", "attributeKey", "dataType",
                unit, "minValid", "maxValid", required, metadata
            ) VALUES ($1, now(), now(), $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT ("platformId", COALESCE("projectId", ''), "attributeKey") DO UPDATE SET
                updated     = now(),
                "dataType"  = EXCLUDED."dataType",
                unit        = EXCLUDED.unit,
                "minValid"  = EXCLUDED."minValid",
                "maxValid"  = EXCLUDED."maxValid",
                required    = EXCLUDED.required,
                metadata    = EXCLUDED.metadata
            "#,
        )
        .bind(&def.id)
        .bind(&def.platform_id)
        .bind(&def.project_id)
        .bind(&def.attribute_key)
        .bind(def.data_type.as_str())
        .bind(&def.unit)
        .bind(def.min_valid)
        .bind(def.max_valid)
        .bind(def.required)
        .bind(Json(&def.metadata))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn query_attribute_defs(
        &self,
        filter: &AttributeDefFilter,
    ) -> Result<Vec<AttributeDef>, StorageError> {
        let mut sql = String::from("SELECT * FROM memory_attribute_registry WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        let mut n = 0;
        macro_rules! add {
            ($col:expr, $val:expr) => {{
                n += 1;
                sql.push_str(&format!(" AND {} = ${}", $col, n));
                binds.push($val);
            }};
        }
        if let Some(s) = &filter.platform_id {
            add!("\"platformId\"", s.clone());
        }
        if let Some(s) = &filter.project_id {
            add!("\"projectId\"", s.clone());
        }
        if let Some(s) = &filter.attribute_key {
            add!("\"attributeKey\"", s.clone());
        }
        sql.push_str(" ORDER BY \"attributeKey\" ASC");
        if let Some(l) = filter.limit {
            sql.push_str(&format!(" LIMIT {}", l));
        }
        if let Some(o) = filter.offset {
            sql.push_str(&format!(" OFFSET {}", o));
        }
        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = qb.bind(b);
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_attribute_def).collect()
    }

    async fn save_typed_observations(
        &self,
        observations: &[TypedObservation],
    ) -> Result<IngestReport, StorageError> {
        let mut report = IngestReport::default();
        if observations.is_empty() {
            return Ok(report);
        }

        // Group by platform so validation uses that tenant's registry and so
        // each tenant's writes commit independently.
        let mut by_platform: BTreeMap<String, Vec<&TypedObservation>> = BTreeMap::new();
        for obs in observations {
            by_platform
                .entry(obs.platform_id.clone())
                .or_default()
                .push(obs);
        }

        for (platform, group) in by_platform {
            // Registry is low-volume; fetch the whole platform's defs once.
            let defs = self
                .query_attribute_defs(&AttributeDefFilter {
                    platform_id: Some(platform.clone()),
                    ..Default::default()
                })
                .await?;
            let index = validate::index_defs(&defs);
            let owned: Vec<TypedObservation> = group.into_iter().cloned().collect();
            let validated = validate::validate_batch(&owned, &index);

            let mut tx = self.pool.begin().await?;
            for (obs, reason) in validated {
                // Persist the row regardless of verdict (quarantined rows are
                // kept for audit). Observations are immutable measurements, so
                // a re-sent id is a no-op at the row level.
                insert_typed_observation(&mut tx, &obs).await?;

                if obs.status == ObservationStatus::Quarantined {
                    report.quarantined += 1;
                    if let Some(r) = reason {
                        report.quarantine_reasons.insert(obs.id.clone(), r);
                    }
                    continue;
                }

                // Accepted. Only numeric values feed an accumulator; other
                // types are stored but have no running stats this increment.
                let Some(value) = obs.value_numeric else {
                    report.accepted += 1;
                    continue;
                };

                // Exactly-once guard: claim the ledger slot. 0 rows affected →
                // this observation was already folded in by another worker /
                // a redelivered message; skip accumulation.
                let claimed = sqlx::query(
                    "INSERT INTO memory_ingest_ledger (\"observationId\") VALUES ($1)
                     ON CONFLICT DO NOTHING",
                )
                .bind(&obs.id)
                .execute(&mut *tx)
                .await?;
                if claimed.rows_affected() == 0 {
                    report.duplicates += 1;
                    continue;
                }

                accumulate(&mut tx, &obs, value).await?;
                report.accepted += 1;
            }
            tx.commit().await?;
        }

        Ok(report)
    }

    async fn query_typed_observations(
        &self,
        filter: &TypedObservationFilter,
    ) -> Result<Vec<TypedObservation>, StorageError> {
        let mut sql = String::from("SELECT * FROM memory_typed_observation WHERE 1=1");
        let mut n = 0;
        // String binds and typed binds are applied in the same order they're
        // pushed; collect them as an enum to preserve ordering.
        enum B {
            S(String),
            F(f64),
            T(DateTime<Utc>),
        }
        let mut binds: Vec<B> = Vec::new();
        macro_rules! add {
            ($col:expr, $val:expr) => {{
                n += 1;
                sql.push_str(&format!(" AND {} {}", $col, format!("${}", n)));
                binds.push($val);
            }};
        }
        if let Some(s) = &filter.platform_id {
            add!("\"platformId\" =", B::S(s.clone()));
        }
        if let Some(s) = &filter.project_id {
            add!("\"projectId\" =", B::S(s.clone()));
        }
        if let Some(s) = &filter.subject_kind {
            add!("\"subjectKind\" =", B::S(s.clone()));
        }
        if let Some(s) = &filter.subject_external_id {
            add!("\"subjectExternalId\" =", B::S(s.clone()));
        }
        if let Some(s) = &filter.attribute_key {
            add!("\"attributeKey\" =", B::S(s.clone()));
        }
        if let Some(s) = &filter.status {
            add!("status =", B::S(s.as_str().to_string()));
        }
        if let Some(t) = filter.since {
            add!("\"observedAt\" >=", B::T(t));
        }
        if let Some(t) = filter.until {
            add!("\"observedAt\" <=", B::T(t));
        }
        if let Some(v) = filter.min_value {
            add!("\"valueNumeric\" >=", B::F(v));
        }
        if let Some(v) = filter.max_value {
            add!("\"valueNumeric\" <=", B::F(v));
        }
        sql.push_str(" ORDER BY \"observedAt\" DESC");
        if let Some(l) = filter.limit {
            sql.push_str(&format!(" LIMIT {}", l));
        }
        if let Some(o) = filter.offset {
            sql.push_str(&format!(" OFFSET {}", o));
        }
        let mut qb = sqlx::query(&sql);
        for b in &binds {
            qb = match b {
                B::S(s) => qb.bind(s),
                B::F(f) => qb.bind(f),
                B::T(t) => qb.bind(t),
            };
        }
        let rows = qb.fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_typed_observation).collect()
    }

    async fn get_accumulator(
        &self,
        key: &AccumulatorKey,
    ) -> Result<Option<Accumulator>, StorageError> {
        let row = sqlx::query(
            r#"SELECT * FROM memory_attribute_accumulator
               WHERE "platformId" = $1 AND "subjectKind" = $2
                 AND "subjectExternalId" = $3 AND "attributeKey" = $4"#,
        )
        .bind(&key.platform_id)
        .bind(&key.subject_kind)
        .bind(&key.subject_external_id)
        .bind(&key.attribute_key)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_accumulator).transpose()
    }

    async fn migrate(&self) -> Result<i64, StorageError> {
        MIGRATIONS
            .run(&self.pool)
            .await
            .map_err(|e| StorageError::Other(anyhow::anyhow!(e)))?;
        let latest: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);
        Ok(latest)
    }

    fn backend(&self) -> &'static str {
        "postgres"
    }
}

/// Build the `AND ...`-prefixed WHERE fragment with numbered placeholders
/// (`$start+1`, `$start+2`, ...) and ordered bind values for a `MemoryFilter`.
/// `start` is the count of placeholders already consumed before the filter
/// (0 for `query`/`items_missing_embedding`; 1 for `vector_search`, where `$1`
/// is the query vector). Returns the fragment, the binds, and the final
/// placeholder index used. Shared so all read paths filter identically.
fn pg_filter_clause(f: &MemoryFilter, start: usize) -> (String, Vec<String>, usize) {
    let mut sql = String::new();
    let mut binds: Vec<String> = Vec::new();
    let mut n = start;

    macro_rules! add {
        ($field_sql:expr, $val:expr) => {{
            n += 1;
            sql.push_str(&format!(" AND {} = ${}", $field_sql, n));
            binds.push($val);
        }};
    }

    if let Some(scope) = f.scope {
        add!("scope", scope.as_str().to_string());
    }
    if let Some(s) = &f.platform_id {
        add!("\"platformId\"", s.clone());
    }
    if let Some(s) = &f.project_id {
        add!("\"projectId\"", s.clone());
    }
    if let Some(s) = &f.agent_id {
        add!("\"chatbotId\"", s.clone());
    }
    if let Some(s) = &f.user_id {
        add!("\"chatIdentityId\"", s.clone());
    }
    if let Some(s) = &f.session_id {
        add!("\"sessionKey\"", s.clone());
    }
    if let Some(status) = f.status {
        add!("status", status.as_str().to_string());
    }
    if let Some(s) = &f.kind {
        add!("type", s.clone());
    }
    if let Some(s) = &f.text_match {
        n += 1;
        sql.push_str(&format!(" AND content ILIKE ${n}"));
        binds.push(format!("%{s}%"));
    }
    (sql, binds, n)
}

/// `SELECT <col>, COUNT(*) ... GROUP BY <col>` into a map. `col` is a fixed
/// identifier supplied by call sites (never user input); `clause` carries the
/// parameterized predicates with their `$n` placeholders.
async fn pg_grouped_counts(
    pool: &PgPool,
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

/// Render an f32 vector as a pgvector text literal, e.g. `[1.5,2,3]`, for
/// binding as text and casting with `::vector`. Avoids a pgvector driver dep.
fn pg_vector_literal(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 2);
    s.push('[');
    for (i, f) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&f.to_string());
    }
    s.push(']');
    s
}

fn row_to_binding_pg(row: PgRow) -> Result<ProjectBinding, StorageError> {
    Ok(ProjectBinding {
        cwd: row.try_get("cwd")?,
        platform_id: row.try_get("platform_id")?,
        project_id: row.try_get("project_id")?,
        created: row.try_get("created")?,
        updated: row.try_get("updated")?,
    })
}

// ────────────────────────────────────────────────────────────
// row → MemoryItem decoder
// ────────────────────────────────────────────────────────────

fn row_to_memory_item(row: PgRow) -> Result<MemoryItem, StorageError> {
    let scope: String = row.try_get("scope")?;
    let status: String = row.try_get("status")?;
    let impact: Option<String> = row.try_get("impact")?;
    let metadata: Option<Json<serde_json::Value>> = row.try_get("metadata")?;

    Ok(MemoryItem {
        id: row.try_get("id")?,
        created: row.try_get("created")?,
        updated: row.try_get("updated")?,
        platform_id: row.try_get("platformId")?,
        project_id: row.try_get("projectId")?,
        location_id: row.try_get("locationId")?,
        chatbot_id: row.try_get("chatbotId")?,
        chat_identity_id: row.try_get("chatIdentityId")?,
        session_key: row.try_get("sessionKey")?,
        type_: row.try_get("type")?,
        content: row.try_get("content")?,
        category: row.try_get("category")?,
        importance: row.try_get("importance")?,
        source: row.try_get("source")?,
        metadata: metadata.map(|j| j.0).unwrap_or(serde_json::Value::Null),
        scope: parse_scope(&scope)?,
        status: parse_status(&status)?,
        confidence: row.try_get("confidence")?,
        impact: impact.as_deref().map(parse_impact).transpose()?,
        superseded_by_id: row.try_get("supersededById")?,
        confirmed_by_user_id: row.try_get("confirmedByUserId")?,
        confirmed_at: row.try_get("confirmedAt")?,
        negative_rating_count: row.try_get("negativeRatingCount")?,
        valid_from: row.try_get("validFrom")?,
        valid_to: row.try_get("validTo")?,
        learned_at: row.try_get("learnedAt")?,
        last_accessed_at: row.try_get("lastAccessedAt")?,
    })
}

fn row_to_audit_event(row: PgRow) -> Result<MemoryAuditEvent, StorageError> {
    let metadata: Option<Json<serde_json::Value>> = row.try_get("metadata")?;
    Ok(MemoryAuditEvent {
        id: row.try_get("id")?,
        created: row.try_get("created")?,
        platform_id: row.try_get("platformId")?,
        project_id: row.try_get("projectId")?,
        actor: row.try_get("actor")?,
        event_type: row.try_get("eventType")?,
        query: row.try_get("query")?,
        scope: row.try_get("scope")?,
        memory_ids: row.try_get("memoryIds")?,
        result_count: row.try_get("resultCount")?,
        metadata: metadata.map(|j| j.0).unwrap_or(serde_json::Value::Null),
    })
}

fn row_to_feedback(row: PgRow) -> Result<MemoryFeedback, StorageError> {
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
        created: row.try_get("created")?,
        updated: row.try_get("updated")?,
        memory_id: row.try_get("memoryId")?,
        response_id: row.try_get("responseId")?,
        rating,
        comment: row.try_get("comment")?,
        created_by_user_id: row.try_get("createdByUserId")?,
    })
}

fn row_to_contact(row: PgRow) -> Result<Contact, StorageError> {
    let metadata: Option<Json<serde_json::Value>> = row.try_get("metadata")?;
    let external_ids: Option<Json<serde_json::Value>> = row.try_get("externalIds")?;
    let tags: Json<serde_json::Value> = row.try_get("tags")?;
    Ok(Contact {
        id: row.try_get("id")?,
        created: row.try_get("created")?,
        updated: row.try_get("updated")?,
        chatbot_id: row.try_get("chatbotId")?,
        project_id: row.try_get("projectId")?,
        name: row.try_get("name")?,
        phone: row.try_get("phone")?,
        email: row.try_get("email")?,
        relationship: row.try_get("relationship")?,
        is_emergency: row.try_get("isEmergency")?,
        priority: row.try_get("priority")?,
        notes: row.try_get("notes")?,
        metadata: metadata.map(|j| j.0).unwrap_or(serde_json::Value::Null),
        external_ids: external_ids.map(|j| j.0).unwrap_or(serde_json::Value::Null),
        tags: tags.0,
        segment: row.try_get("segment")?,
        // lifetimeValue is declared `decimal(12,2)`; convert via PgNumeric ->
        // f64 if present.
        lifetime_value: row
            .try_get::<Option<sqlx::types::BigDecimal>, _>("lifetimeValue")
            .ok()
            .flatten()
            .and_then(|bd| {
                use std::str::FromStr;
                f64::from_str(&bd.to_string()).ok()
            }),
        last_interaction_at: row.try_get("lastInteractionAt")?,
        chat_identity_id: row.try_get("chatIdentityId")?,
    })
}

fn row_to_contact_event(row: PgRow) -> Result<ContactEvent, StorageError> {
    let event_data: Option<Json<serde_json::Value>> = row.try_get("eventData")?;
    Ok(ContactEvent {
        id: row.try_get("id")?,
        created: row.try_get("created")?,
        updated: row.try_get("updated")?,
        contact_id: row.try_get("contactId")?,
        project_id: row.try_get("projectId")?,
        chatbot_id: row.try_get("chatbotId")?,
        event_type: row.try_get("eventType")?,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        event_data: event_data.map(|j| j.0).unwrap_or(serde_json::Value::Null),
        occurred_at: row.try_get("occurredAt")?,
        source: row.try_get("source")?,
        engagement_rule_id: row.try_get("engagementRuleId")?,
        engagement_execution_id: row.try_get("engagementExecutionId")?,
    })
}

fn row_to_block(row: PgRow) -> Result<MemoryBlock, StorageError> {
    let scope: String = row.try_get("scope")?;
    Ok(MemoryBlock {
        id: row.try_get("id")?,
        created: row.try_get("created")?,
        updated: row.try_get("updated")?,
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
        importance: row.try_get("importance")?,
        last_edited_by: row.try_get("lastEditedBy")?,
        last_edited_at: row.try_get("lastEditedAt")?,
        version: row.try_get("version")?,
    })
}

fn row_to_entity(row: PgRow) -> Result<MemoryEntity, StorageError> {
    let scope: String = row.try_get("scope")?;
    let aliases: Vec<String> = row.try_get("aliases")?;
    let metadata: Option<Json<serde_json::Value>> = row.try_get("metadata")?;
    Ok(MemoryEntity {
        id: row.try_get("id")?,
        created: row.try_get("created")?,
        updated: row.try_get("updated")?,
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
        metadata: metadata.map(|j| j.0).unwrap_or(serde_json::Value::Null),
        valid_from: row.try_get("validFrom")?,
        valid_to: row.try_get("validTo")?,
        superseded_by_id: row.try_get("supersededById")?,
    })
}

fn row_to_edge(row: PgRow) -> Result<MemoryEdge, StorageError> {
    let scope: String = row.try_get("scope")?;
    let metadata: Option<Json<serde_json::Value>> = row.try_get("metadata")?;
    Ok(MemoryEdge {
        id: row.try_get("id")?,
        created: row.try_get("created")?,
        updated: row.try_get("updated")?,
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
        weight: row.try_get("weight")?,
        source_memory_id: row.try_get("sourceMemoryId")?,
        metadata: metadata.map(|j| j.0).unwrap_or(serde_json::Value::Null),
        valid_from: row.try_get("validFrom")?,
        valid_to: row.try_get("validTo")?,
    })
}

fn row_to_user_profile(row: PgRow) -> Result<UserProfile, StorageError> {
    let profile: Json<serde_json::Value> = row.try_get("profile")?;
    Ok(UserProfile {
        id: row.try_get("id")?,
        created: row.try_get("created")?,
        updated: row.try_get("updated")?,
        chat_identity_id: row.try_get("chatIdentityId")?,
        project_id: row.try_get("projectId")?,
        platform_id: row.try_get("platformId")?,
        profile: profile.0,
        profile_version: row.try_get("profileVersion")?,
        last_updated_from_session_id: row.try_get("lastUpdatedFromSessionId")?,
    })
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

// Type aliases for unused-variable readability — keeps cargo doc happy.
#[allow(dead_code)]
type _Used = DateTime<Utc>;

// ────────────────────────────────────────────────────────────
// Tests — gated on DATABASE_URL pointing to a Postgres instance.
// ────────────────────────────────────────────────────────────

// ── typed-attribute helpers ─────────────────────────────────

/// Persist one typed observation. Idempotent at the row level: a re-sent id is
/// a no-op (measurements are immutable). Runs inside the ingest transaction.
async fn insert_typed_observation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    obs: &TypedObservation,
) -> Result<(), StorageError> {
    sqlx::query(
        r#"
        INSERT INTO memory_typed_observation (
            id, "platformId", "projectId", "subjectKind", "subjectExternalId",
            "attributeKey", "valueNumeric", "valueText", "valueBool", "valueTs",
            "observedAt", source, trust, "qualityScore", status, "ingestedAt"
        ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15, now())
        ON CONFLICT (id, "platformId") DO NOTHING
        "#,
    )
    .bind(&obs.id)
    .bind(&obs.platform_id)
    .bind(&obs.project_id)
    .bind(&obs.subject_kind)
    .bind(&obs.subject_external_id)
    .bind(&obs.attribute_key)
    .bind(obs.value_numeric)
    .bind(&obs.value_text)
    .bind(obs.value_bool)
    .bind(obs.value_ts)
    .bind(obs.observed_at)
    .bind(&obs.source)
    .bind(obs.trust)
    .bind(obs.quality_score)
    .bind(obs.status.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Fold one accepted numeric observation into its `(subject, attribute)`
/// accumulator. The whole update is a single atomic upsert — references to the
/// existing row on the right-hand side see pre-update values — so it is correct
/// under concurrent writers from multiple engine replicas without any lock.
/// `count`/`sum`/`sumSq` accumulate the raw moments (mean/variance derived on
/// read); `cumulative` is the running total (cumulative-to-limit substrate);
/// `ewma`/`ewmaVar` are an exponentially-weighted mean/variance (alpha = 0.3).
async fn accumulate(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    obs: &TypedObservation,
    value: f64,
) -> Result<(), StorageError> {
    sqlx::query(
        r#"
        INSERT INTO memory_attribute_accumulator (
            "platformId","subjectKind","subjectExternalId","attributeKey","projectId",
            count, sum, "sumSq", "minVal","maxVal","lastVal","lastObservedAt",
            cumulative, ewma, "ewmaVar", updated
        ) VALUES ($1,$2,$3,$4,$5, 1, $6, $6*$6, $6, $6, $6, $7, $6, $6, 0, now())
        ON CONFLICT ("platformId","subjectKind","subjectExternalId","attributeKey") DO UPDATE SET
            count            = memory_attribute_accumulator.count + 1,
            sum              = memory_attribute_accumulator.sum + EXCLUDED.sum,
            "sumSq"          = memory_attribute_accumulator."sumSq" + EXCLUDED."sumSq",
            "minVal"         = LEAST(memory_attribute_accumulator."minVal", EXCLUDED."minVal"),
            "maxVal"         = GREATEST(memory_attribute_accumulator."maxVal", EXCLUDED."maxVal"),
            cumulative       = memory_attribute_accumulator.cumulative + EXCLUDED.sum,
            "lastVal"        = CASE
                                 WHEN EXCLUDED."lastObservedAt" >= memory_attribute_accumulator."lastObservedAt"
                                 THEN EXCLUDED."lastVal"
                                 ELSE memory_attribute_accumulator."lastVal"
                               END,
            "lastObservedAt" = GREATEST(memory_attribute_accumulator."lastObservedAt", EXCLUDED."lastObservedAt"),
            ewma             = 0.3 * EXCLUDED.ewma
                               + 0.7 * COALESCE(memory_attribute_accumulator.ewma, EXCLUDED.ewma),
            "ewmaVar"        = 0.7 * (
                                 COALESCE(memory_attribute_accumulator."ewmaVar", 0)
                                 + 0.3 * power(
                                     EXCLUDED.ewma
                                     - COALESCE(memory_attribute_accumulator.ewma, EXCLUDED.ewma),
                                     2)
                               ),
            updated          = now()
        "#,
    )
    .bind(&obs.platform_id)
    .bind(&obs.subject_kind)
    .bind(&obs.subject_external_id)
    .bind(&obs.attribute_key)
    .bind(&obs.project_id)
    .bind(value) // $6
    .bind(obs.observed_at) // $7
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn row_to_typed_observation(row: PgRow) -> Result<TypedObservation, StorageError> {
    let status: String = row.try_get("status")?;
    let status = match status.as_str() {
        "quarantined" => ObservationStatus::Quarantined,
        _ => ObservationStatus::Accepted,
    };
    Ok(TypedObservation {
        id: row.try_get("id")?,
        platform_id: row.try_get("platformId")?,
        project_id: row.try_get("projectId")?,
        subject_kind: row.try_get("subjectKind")?,
        subject_external_id: row.try_get("subjectExternalId")?,
        attribute_key: row.try_get("attributeKey")?,
        value_numeric: row.try_get("valueNumeric")?,
        value_text: row.try_get("valueText")?,
        value_bool: row.try_get("valueBool")?,
        value_ts: row.try_get("valueTs")?,
        observed_at: row.try_get("observedAt")?,
        source: row.try_get("source")?,
        trust: row.try_get("trust")?,
        quality_score: row.try_get("qualityScore")?,
        status,
        ingested_at: row.try_get("ingestedAt")?,
    })
}

fn row_to_attribute_def(row: PgRow) -> Result<AttributeDef, StorageError> {
    let dt: String = row.try_get("dataType")?;
    let data_type = DataType::parse(&dt)
        .ok_or_else(|| StorageError::SchemaMismatch(format!("unknown dataType '{dt}'")))?;
    let metadata: Option<Json<serde_json::Value>> = row.try_get("metadata")?;
    Ok(AttributeDef {
        id: row.try_get("id")?,
        platform_id: row.try_get("platformId")?,
        project_id: row.try_get("projectId")?,
        attribute_key: row.try_get("attributeKey")?,
        data_type,
        unit: row.try_get("unit")?,
        min_valid: row.try_get("minValid")?,
        max_valid: row.try_get("maxValid")?,
        required: row.try_get("required")?,
        metadata: metadata.map(|j| j.0).unwrap_or(serde_json::Value::Null),
    })
}

fn row_to_accumulator(row: PgRow) -> Result<Accumulator, StorageError> {
    Ok(Accumulator {
        platform_id: row.try_get("platformId")?,
        subject_kind: row.try_get("subjectKind")?,
        subject_external_id: row.try_get("subjectExternalId")?,
        attribute_key: row.try_get("attributeKey")?,
        project_id: row.try_get("projectId")?,
        count: row.try_get("count")?,
        sum: row.try_get("sum")?,
        sum_sq: row.try_get("sumSq")?,
        min_val: row.try_get("minVal")?,
        max_val: row.try_get("maxVal")?,
        last_val: row.try_get("lastVal")?,
        last_observed_at: row.try_get("lastObservedAt")?,
        cumulative: row.try_get("cumulative")?,
        ewma: row.try_get("ewma")?,
        ewma_var: row.try_get("ewmaVar")?,
        updated: row.try_get("updated")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::MemoryScope;

    /// Skip silently if `DATABASE_URL` isn't set. CI provides one; local
    /// dev opts in. This keeps `cargo test` green on a fresh checkout.
    async fn store_or_skip() -> Option<PostgresStore> {
        let url = std::env::var("THINKFLEET_TEST_PG_URL").ok()?;
        let s = PostgresStore::connect(&url).await.ok()?;
        s.migrate().await.ok()?;
        Some(s)
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

    /// Real pgvector path: runs only when `THINKFLEET_TEST_PG_URL` points at a
    /// Postgres with the `vector` extension (CI provides one; skipped locally).
    /// Verifies HNSW cosine ordering + filter scoping end-to-end.
    #[tokio::test]
    async fn vector_search_ranks_by_cosine_and_respects_filter_pg() {
        let Some(s) = store_or_skip().await else {
            return;
        };
        // The pgvector column is vector(1536); build sparse unit vectors.
        let unit = |idx: usize| {
            let mut v = vec![0.0f32; 1536];
            v[idx] = 1.0;
            v
        };
        let mut a = new_item("memtfe_vec_a");
        a.project_id = Some("memtfe_p".into());
        let mut b = new_item("memtfe_vec_b");
        b.project_id = Some("memtfe_p".into());
        let mut c = new_item("memtfe_vec_c");
        c.project_id = Some("memtfe_other".into());
        s.save(&a).await.unwrap();
        s.save(&b).await.unwrap();
        s.save(&c).await.unwrap();

        s.set_embedding("memtfe_vec_a", &unit(0)).await.unwrap();
        s.set_embedding("memtfe_vec_b", &unit(1)).await.unwrap();
        // Same direction as `a` but a different project — must be filtered out.
        s.set_embedding("memtfe_vec_c", &unit(0)).await.unwrap();

        let filter = crate::MemoryFilter {
            project_id: Some("memtfe_p".into()),
            ..Default::default()
        };
        let hits = s.vector_search(&unit(0), &filter, 10).await.unwrap();
        assert_eq!(hits.len(), 2, "only project-p items with embeddings");
        assert_eq!(hits[0].item.id, "memtfe_vec_a", "closest first");
        assert!(hits[0].similarity > hits[1].similarity);
        assert!((hits[0].similarity - 1.0).abs() < 1e-4);

        for id in ["memtfe_vec_a", "memtfe_vec_b", "memtfe_vec_c"] {
            sqlx::query("DELETE FROM clawdbot_memory_item WHERE id = $1")
                .bind(id)
                .execute(s.pool())
                .await
                .ok();
        }
    }

    #[tokio::test]
    async fn save_then_get_roundtrip() {
        let Some(s) = store_or_skip().await else {
            return;
        };
        let item = new_item("memtfe_save_get");
        s.save(&item).await.unwrap();
        let got = s.get("memtfe_save_get").await.unwrap().unwrap();
        assert_eq!(got.content, item.content);
        // Cleanup
        sqlx::query("DELETE FROM clawdbot_memory_item WHERE id = $1")
            .bind("memtfe_save_get")
            .execute(s.pool())
            .await
            .ok();
    }

    // ── typed-attribute integration tests ───────────────────
    //
    // These exercise the real Postgres path (registry, partitioned observation
    // table, ledger-guarded accumulation). They run only when
    // THINKFLEET_TEST_PG_URL is set, like the rest of this module.

    use memory_core::{AttributeDef, DataType, ObservationStatus, TypedObservation};

    async fn register_credit_def(s: &PostgresStore, platform: &str) {
        s.upsert_attribute_def(&AttributeDef {
            id: format!("{platform}_creddef"),
            platform_id: platform.into(),
            project_id: None,
            attribute_key: "credit_score".into(),
            data_type: DataType::Numeric,
            unit: None,
            min_valid: Some(300.0),
            max_valid: Some(850.0),
            required: false,
            metadata: serde_json::Value::Null,
        })
        .await
        .unwrap();
    }

    async fn cleanup_typed(s: &PostgresStore, platform: &str, ids: &[&str]) {
        for id in ids {
            sqlx::query("DELETE FROM memory_ingest_ledger WHERE \"observationId\" = $1")
                .bind(id)
                .execute(s.pool())
                .await
                .ok();
        }
        sqlx::query("DELETE FROM memory_typed_observation WHERE \"platformId\" = $1")
            .bind(platform)
            .execute(s.pool())
            .await
            .ok();
        sqlx::query("DELETE FROM memory_attribute_accumulator WHERE \"platformId\" = $1")
            .bind(platform)
            .execute(s.pool())
            .await
            .ok();
        sqlx::query("DELETE FROM memory_attribute_registry WHERE \"platformId\" = $1")
            .bind(platform)
            .execute(s.pool())
            .await
            .ok();
    }

    fn obs(id: &str, platform: &str, value: f64) -> TypedObservation {
        TypedObservation::numeric(
            id,
            platform,
            "contact",
            "c1",
            "credit_score",
            value,
            Utc::now(),
        )
    }

    #[tokio::test]
    async fn typed_ingest_validates_accumulates_and_dedupes_pg() {
        let Some(s) = store_or_skip().await else {
            return;
        };
        let platform = "memtfe_typed_a";
        cleanup_typed(&s, platform, &["memtfe_o1", "memtfe_o2", "memtfe_o3"]).await;
        register_credit_def(&s, platform).await;

        // o1 valid, o2 out-of-range (quarantined), o3 valid.
        let batch = vec![
            obs("memtfe_o1", platform, 600.0),
            obs("memtfe_o2", platform, 9000.0),
            obs("memtfe_o3", platform, 700.0),
        ];
        let report = s.save_typed_observations(&batch).await.unwrap();
        assert_eq!(report.accepted, 2, "two in-range rows accepted");
        assert_eq!(report.quarantined, 1, "9000 quarantined");
        assert!(report.quarantine_reasons.contains_key("memtfe_o2"));

        // Accumulator reflects only the two accepted values: mean 650.
        let acc = s
            .get_accumulator(&AccumulatorKey {
                platform_id: platform.into(),
                subject_kind: "contact".into(),
                subject_external_id: "c1".into(),
                attribute_key: "credit_score".into(),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(acc.count, 2);
        assert!((acc.mean().unwrap() - 650.0).abs() < 1e-9);
        assert_eq!(acc.min_val, Some(600.0));
        assert_eq!(acc.max_val, Some(700.0));

        // Re-sending the same batch must NOT double-count (ledger dedupe).
        let report2 = s.save_typed_observations(&batch).await.unwrap();
        assert_eq!(report2.duplicates, 2, "both accepted rows already applied");
        assert_eq!(report2.accepted, 0);
        let acc2 = s
            .get_accumulator(&AccumulatorKey {
                platform_id: platform.into(),
                subject_kind: "contact".into(),
                subject_external_id: "c1".into(),
                attribute_key: "credit_score".into(),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(acc2.count, 2, "count unchanged after redelivery");

        // Range query returns only accepted rows in [600,700].
        let rows = s
            .query_typed_observations(&TypedObservationFilter {
                platform_id: Some(platform.into()),
                attribute_key: Some("credit_score".into()),
                status: Some(ObservationStatus::Accepted),
                min_value: Some(600.0),
                max_value: Some(700.0),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);

        cleanup_typed(&s, platform, &["memtfe_o1", "memtfe_o2", "memtfe_o3"]).await;
    }

    #[tokio::test]
    async fn typed_ingest_accuracy_vs_fold_reference_pg() {
        // Ingest thousands of generated observations through the real SQL path,
        // then assert the persisted accumulator matches the in-Rust `fold`
        // reference value-for-value — proving the SQL arithmetic is correct at
        // volume, not just on the hand-picked cases.
        let Some(s) = store_or_skip().await else {
            return;
        };
        let platform = "memtfe_typed_acc";
        let total = 3_000usize;
        let ids: Vec<String> = (0..total).map(|i| format!("memtfe_acc_{i}")).collect();
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        // Clean in chunks (avoid a giant IN list); platform-wide deletes handle the rest.
        cleanup_typed(&s, platform, &[]).await;
        register_credit_def(&s, platform).await;

        // Deterministic LCG stream in [300,850] so all rows pass validation.
        let mut state: u64 = 0xACC0_1234;
        let mut next = || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
        };

        let mut reference =
            memory_core::Accumulator::empty(platform, "contact", "c1", "credit_score");
        let mut batch = Vec::with_capacity(total);
        for (i, id) in ids.iter().enumerate() {
            let value = (300.0 + next() * 550.0).round();
            let observed = DateTime::<Utc>::from_timestamp(1_700_000_000 + i as i64, 0).unwrap();
            let mut o = TypedObservation::numeric(
                id,
                platform,
                "contact",
                "c1",
                "credit_score",
                value,
                observed,
            );
            o.observed_at = observed;
            reference.fold(value, observed);
            batch.push(o);
        }
        // Ingest in chunks to mimic batched delivery.
        for chunk in batch.chunks(500) {
            s.save_typed_observations(chunk).await.unwrap();
        }

        let acc = s
            .get_accumulator(&AccumulatorKey {
                platform_id: platform.into(),
                subject_kind: "contact".into(),
                subject_external_id: "c1".into(),
                attribute_key: "credit_score".into(),
            })
            .await
            .unwrap()
            .unwrap();

        assert_eq!(acc.count, total as i64);
        assert!((acc.sum - reference.sum).abs() < 1e-3, "sum");
        assert!((acc.sum_sq - reference.sum_sq).abs() < 1.0, "sumSq");
        assert_eq!(acc.min_val, reference.min_val, "min");
        assert_eq!(acc.max_val, reference.max_val, "max");
        assert!(
            (acc.mean().unwrap() - reference.mean().unwrap()).abs() < 1e-6,
            "mean"
        );
        assert!(
            (acc.variance().unwrap() - reference.variance().unwrap()).abs() < 1e-3,
            "variance"
        );
        assert_eq!(acc.last_val, reference.last_val, "last");

        for id in &id_refs[..total.min(id_refs.len())] {
            sqlx::query("DELETE FROM memory_ingest_ledger WHERE \"observationId\" = $1")
                .bind(id)
                .execute(s.pool())
                .await
                .ok();
        }
        cleanup_typed(&s, platform, &[]).await;
    }

    #[tokio::test]
    async fn typed_ingest_is_concurrency_safe_pg() {
        let Some(s) = store_or_skip().await else {
            return;
        };
        let platform = "memtfe_typed_conc";
        let ids: Vec<String> = (0..20).map(|i| format!("memtfe_c{i}")).collect();
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        cleanup_typed(&s, platform, &id_refs).await;
        register_credit_def(&s, platform).await;

        let batch: Vec<TypedObservation> = ids.iter().map(|id| obs(id, platform, 650.0)).collect();

        // Apply the SAME batch from two tasks at once. The ledger must ensure
        // each observation is folded in exactly once → count == 20, not 40.
        let s = std::sync::Arc::new(s);
        let (b1, b2) = (batch.clone(), batch.clone());
        let (s1, s2) = (s.clone(), s.clone());
        let h1 = tokio::spawn(async move { s1.save_typed_observations(&b1).await });
        let h2 = tokio::spawn(async move { s2.save_typed_observations(&b2).await });
        h1.await.unwrap().unwrap();
        h2.await.unwrap().unwrap();

        let acc = s
            .get_accumulator(&AccumulatorKey {
                platform_id: platform.into(),
                subject_kind: "contact".into(),
                subject_external_id: "c1".into(),
                attribute_key: "credit_score".into(),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(acc.count, 20, "exactly-once under concurrent writers");

        cleanup_typed(&s, platform, &id_refs).await;
    }
}
