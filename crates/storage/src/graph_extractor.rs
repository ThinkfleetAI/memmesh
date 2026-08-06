// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Self-wiring knowledge graph from observed text — zero LLM.
//!
//! After `observe::observe` writes a memory item, this module scans the
//! same text for entity mentions and typed relationships, resolves each
//! to a `MemoryEntity` (creating new ones as needed via
//! `entity_resolver`), and writes typed `MemoryEdge` rows linking them.
//!
//! Designed for high precision over recall. The patterns it matches are
//! deterministic and conservative — we'd rather miss a mention than
//! pollute the graph with false positives that downstream ranking has
//! to compensate for.
//!
//! ## What gets extracted
//!
//! **Entity mentions** (each becomes a `MemoryEntity` of type `person`
//! or `org`):
//!
//! - **Email addresses** → person (high precision)
//! - **`@handle`** → person (high precision; matches `@\w+`)
//! - **Org-suffix phrases** (`Acme Inc`, `Foo Corp`, etc.) → org
//!
//! Capitalized free-text names (e.g. `Kevin Smith`) are NOT extracted
//! on their own — too many false positives ("Send Email" in step
//! descriptions, "Microsoft Outlook" piece names, etc.). They DO get
//! extracted when they appear as the subject/object of a recognized
//! relationship verb.
//!
//! **Typed edges** (subject + predicate + object, all resolved to
//! entities):
//!
//! - `X works at Y`     → predicate `works_at`
//! - `X founded Y`       → predicate `founded`
//! - `X invested in Y`   → predicate `invested_in`
//! - `X advises Y`       → predicate `advises`
//! - `X met (with) Y`    → predicate `met`
//! - `X attended Y`      → predicate `attended`
//! - `X joined Y`        → predicate `joined`
//! - `X left Y`          → predicate `left`
//!
//! Each edge carries `source_memory_id` so provenance survives —
//! "where did we learn that?" stays answerable.

use chrono::Utc;
use memory_core::{MemoryEdge, MemoryEntity, MemoryScope};
use once_cell::sync::Lazy;
use regex::Regex;
use uuid::Uuid;

use crate::entity_resolver::{add_alias, resolve_or_create_entity, EntityContext};
use crate::{Storage, StorageError};

/// Per-call context — what platform/project/scope to attribute new
/// entities to, plus the source memory id to stamp on edges.
#[derive(Debug, Clone)]
pub struct GraphContext {
    pub platform_id: String,
    pub project_id: Option<String>,
    pub scope: MemoryScope,
    /// Memory item this text came from. Stamped on every edge as
    /// `source_memory_id` for provenance.
    pub source_memory_id: String,
}

/// Counts returned by `extract_and_wire`. Useful for observability and
/// for the eval framework to score graph density.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GraphExtractionStats {
    pub entities_resolved: usize,
    pub edges_written: usize,
}

// ─── Patterns ────────────────────────────────────────────────────────

static EMAIL_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b").unwrap()
});

static MENTION_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:^|[\s,.;:!?\(])@([A-Za-z][A-Za-z0-9_\-]{1,30})").unwrap()
});

static ORG_SUFFIX_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"\b((?:[A-Z][A-Za-z0-9&]+\s+){0,3}[A-Z][A-Za-z0-9&]+)\s+(Inc|LLC|Corp|Co|Ltd|GmbH|S\.A|PLC|AG)\b",
    )
    .unwrap()
});

static RELATION_PATTERNS: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    let name = r"([A-Z][A-Za-z0-9._\-]+(?:\s+[A-Z][A-Za-z0-9._\-]+){0,3})";
    let preds: &[(&str, &str)] = &[
        (r"\bworks?\s+at\b", "works_at"),
        (r"\bfounded\b", "founded"),
        (r"\binvested\s+in\b", "invested_in"),
        (r"\badvises\b", "advises"),
        (r"\bmet\s+(?:with\s+)?", "met"),
        (r"\battended\b", "attended"),
        (r"\bjoined\b", "joined"),
        (r"\bleft\b", "left"),
    ];
    preds
        .iter()
        .map(|(verb, pred)| {
            let pattern = format!(r"{name}\s+{verb}\s+{name}");
            (Regex::new(&pattern).unwrap(), *pred)
        })
        .collect()
});

// ─── Public entry point ─────────────────────────────────────────────

pub async fn extract_and_wire<S: Storage>(
    storage: &S,
    text: &str,
    ctx: &GraphContext,
) -> Result<GraphExtractionStats, StorageError> {
    let mut stats = GraphExtractionStats::default();

    for email in EMAIL_PATTERN.find_iter(text) {
        let addr = email.as_str();
        let entity_ctx = person_ctx(ctx);
        let _entity = resolve_or_create_entity(storage, addr, &entity_ctx).await?;
        stats.entities_resolved += 1;
    }

    for cap in MENTION_PATTERN.captures_iter(text) {
        if let Some(handle) = cap.get(1) {
            let name = handle.as_str();
            let entity_ctx = person_ctx(ctx);
            let _entity = resolve_or_create_entity(storage, name, &entity_ctx).await?;
            stats.entities_resolved += 1;
        }
    }

    for cap in ORG_SUFFIX_PATTERN.captures_iter(text) {
        let full = cap.get(0).unwrap().as_str();
        let entity_ctx = org_ctx(ctx);
        let entity = resolve_or_create_entity(storage, full, &entity_ctx).await?;
        if let Some(short) = cap.get(1) {
            let short_name = short.as_str();
            if short_name != full {
                add_alias(storage, &entity.id, short_name).await?;
            }
        }
        stats.entities_resolved += 1;
    }

    for (re, predicate) in RELATION_PATTERNS.iter() {
        for cap in re.captures_iter(text) {
            let (Some(subj), Some(obj)) = (cap.get(1), cap.get(2)) else {
                continue;
            };
            let subj_entity =
                resolve_or_create_entity(storage, subj.as_str(), &person_ctx(ctx)).await?;
            let obj_ctx = if ORG_SUFFIX_PATTERN.is_match(obj.as_str()) {
                org_ctx(ctx)
            } else {
                person_ctx(ctx)
            };
            let obj_entity =
                resolve_or_create_entity(storage, obj.as_str(), &obj_ctx).await?;
            stats.entities_resolved += 2;

            let edge = build_edge(ctx, &subj_entity.id, predicate, &obj_entity.id);
            storage.save_edge(&edge).await?;
            stats.edges_written += 1;
        }
    }

    Ok(stats)
}

fn person_ctx(ctx: &GraphContext) -> EntityContext {
    EntityContext {
        platform_id: ctx.platform_id.clone(),
        project_id: ctx.project_id.clone(),
        scope: ctx.scope,
        default_type: "person".to_string(),
    }
}

fn org_ctx(ctx: &GraphContext) -> EntityContext {
    EntityContext {
        platform_id: ctx.platform_id.clone(),
        project_id: ctx.project_id.clone(),
        scope: ctx.scope,
        default_type: "org".to_string(),
    }
}

fn build_edge(
    ctx: &GraphContext,
    subject_id: &str,
    predicate: &str,
    object_id: &str,
) -> MemoryEdge {
    let now = Utc::now();
    MemoryEdge {
        id: short_id(),
        created: now,
        updated: now,
        platform_id: ctx.platform_id.clone(),
        project_id: ctx.project_id.clone(),
        location_id: None,
        chatbot_id: None,
        chat_identity_id: None,
        scope: ctx.scope,
        subject_id: subject_id.to_string(),
        predicate: predicate.to_string(),
        object_id: Some(object_id.to_string()),
        object_literal: None,
        weight: 1.0,
        source_memory_id: Some(ctx.source_memory_id.clone()),
        metadata: serde_json::Value::Null,
        valid_from: now,
        valid_to: None,
    }
}

fn short_id() -> String {
    let raw = Uuid::now_v7().simple().to_string();
    raw.chars().take(21).collect()
}

// Silence unused-import warning when neither test nor sqlite is on; the
// `MemoryEntity` import is referenced in test bodies and helper types.
#[allow(dead_code)]
fn _entity_type_assertion(_: &MemoryEntity) {}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStore;
    use crate::EdgeFilter;
    use memory_core::{MemoryImpact, MemoryItem, MemoryStatus};

    async fn make_store() -> SqliteStore {
        let s = SqliteStore::connect("sqlite::memory:").await.unwrap();
        s.migrate().await.unwrap();
        // The graph extractor stamps every edge with `source_memory_id`,
        // which is a FK into `clawdbot_memory_item`. Seeding the source
        // row up-front lets the tests focus on the extractor's behavior
        // instead of the FK plumbing.
        let now = Utc::now();
        s.save(&MemoryItem {
            id: "src_mem_1".into(),
            created: now,
            updated: now,
            platform_id: "p1".into(),
            project_id: Some("proj_a".into()),
            location_id: None,
            chatbot_id: None,
            chat_identity_id: None,
            session_key: None,
            type_: "observation".into(),
            content: "seed".into(),
            category: None,
            importance: 5.0,
            source: Some("test-seed".into()),
            metadata: serde_json::Value::Null,
            scope: MemoryScope::Project,
            status: MemoryStatus::Confirmed,
            confidence: 1.0,
            impact: Some(MemoryImpact::Low),
            superseded_by_id: None,
            confirmed_by_user_id: None,
            confirmed_at: None,
            negative_rating_count: 0,
            valid_from: now,
            valid_to: None,
            learned_at: now,
            last_accessed_at: now,
        })
        .await
        .unwrap();
        s
    }

    fn ctx() -> GraphContext {
        GraphContext {
            platform_id: "p1".into(),
            project_id: Some("proj_a".into()),
            scope: MemoryScope::Project,
            source_memory_id: "src_mem_1".into(),
        }
    }

    #[tokio::test]
    async fn extracts_emails_as_person_entities() {
        let s = make_store().await;
        let stats = extract_and_wire(
            &s,
            "Met with kevin@gotolig.com about the deal",
            &ctx(),
        )
        .await
        .unwrap();
        assert_eq!(stats.entities_resolved, 1);

        let entities = s
            .query_entities(&crate::EntityFilter {
                platform_id: Some("p1".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].canonical_name, "kevin@gotolig.com");
        assert_eq!(entities[0].type_, "person");
    }

    #[tokio::test]
    async fn extracts_at_mentions() {
        let s = make_store().await;
        let stats = extract_and_wire(
            &s,
            "Talked to @kevin and @melissa about the quarterly plan",
            &ctx(),
        )
        .await
        .unwrap();
        assert_eq!(stats.entities_resolved, 2);
    }

    #[tokio::test]
    async fn org_suffix_creates_org_entity_with_short_alias() {
        let s = make_store().await;
        extract_and_wire(&s, "Closed Series A with Acme Inc last week", &ctx())
            .await
            .unwrap();
        let entities = s
            .query_entities(&crate::EntityFilter {
                platform_id: Some("p1".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].canonical_name, "Acme Inc");
        assert_eq!(entities[0].type_, "org");
        assert_eq!(entities[0].aliases, vec!["Acme".to_string()]);
    }

    #[tokio::test]
    async fn typed_edge_works_at() {
        let s = make_store().await;
        extract_and_wire(&s, "Sarah Chen works at Acme Inc", &ctx())
            .await
            .unwrap();

        let edges = s
            .query_edges(&EdgeFilter {
                platform_id: Some("p1".into()),
                predicate: Some("works_at".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].predicate, "works_at");
        assert_eq!(edges[0].source_memory_id.as_deref(), Some("src_mem_1"));
    }

    #[tokio::test]
    async fn no_false_positives_on_prose() {
        let s = make_store().await;
        let stats = extract_and_wire(
            &s,
            "The codebase uses pnpm for package management. Send Email is the step name.",
            &ctx(),
        )
        .await
        .unwrap();
        assert_eq!(stats.edges_written, 0);

        let entities = s
            .query_entities(&crate::EntityFilter {
                platform_id: Some("p1".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(entities.is_empty(), "expected no entities, got {entities:?}");
    }

    #[tokio::test]
    async fn alias_lookup_resolves_followup_mention() {
        let s = make_store().await;
        extract_and_wire(&s, "Sarah Chen works at Acme Inc", &ctx())
            .await
            .unwrap();
        extract_and_wire(&s, "Sarah Chen left Acme", &ctx()).await.unwrap();

        let entities = s
            .query_entities(&crate::EntityFilter {
                platform_id: Some("p1".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let names: Vec<&str> = entities.iter().map(|e| e.canonical_name.as_str()).collect();
        assert!(names.contains(&"Sarah Chen"));
        assert!(names.contains(&"Acme Inc"));
        assert_eq!(entities.len(), 2, "split-brain entities: {names:?}");
    }
}
