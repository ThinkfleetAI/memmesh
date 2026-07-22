// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Temporal knowledge-graph primitives.
//!
//! Edges are bi-temporal: each carries `valid_from` / `valid_to`, so the graph
//! can answer "what did we believe on date X" (via [`EdgeFilter::as_of`]) — not
//! just "what is true now". What was missing was *maintaining* that timeline:
//! when a subject's single-valued fact changes ("lives in Boston" → "lives in
//! Denver"), the old edge must be *closed* at the moment the new one opens, so
//! it stays in history but stops being current.
//!
//! [`supersede_conflicting_edges`] does exactly that: it's the edge-level
//! analogue of belief revision (which supersedes contradicted memory *items*).
//! Given a newly-asserted edge for a functional predicate, it closes any
//! currently-open edge with the same subject + predicate but a different
//! object, at the new edge's `valid_from`. The result is a contiguous,
//! non-overlapping timeline per (subject, predicate): exactly one edge is
//! current, and every past belief remains queryable at its own `as_of`.

use std::env;

use memory_core::graph::MemoryEdge;

use crate::query::EdgeFilter;
use crate::{Storage, StorageError};

/// Auto-supersession on edge writes is gated: the timeline maintenance only
/// runs when `MEMORY_TEMPORAL_SUPERSEDE_ENABLED=true`, so enabling it is an
/// explicit, reversible decision (default off = no behavior change).
pub fn supersede_enabled() -> bool {
    env::var("MEMORY_TEMPORAL_SUPERSEDE_ENABLED")
        .ok()
        .as_deref()
        == Some("true")
}

/// Whether `predicate` is FUNCTIONAL (single-valued: a subject has one current
/// value, so a new value supersedes the old). Only these are auto-superseded —
/// multi-valued predicates (`likes`, `invested_in`, `attended`) legitimately
/// have many objects and must never be collapsed. Override the set via
/// `MEMORY_TEMPORAL_FUNCTIONAL_PREDICATES` (comma-separated); the default is
/// deliberately conservative.
pub fn is_functional_predicate(predicate: &str) -> bool {
    match env::var("MEMORY_TEMPORAL_FUNCTIONAL_PREDICATES") {
        Ok(list) if !list.trim().is_empty() => {
            list.split(',').map(str::trim).any(|p| p == predicate)
        }
        _ => matches!(
            predicate,
            "works_at" | "lives_in" | "located_in" | "based_in" | "reports_to" | "headquartered_in"
        ),
    }
}

/// Same object target? Edges point at either an entity id or a literal
/// (never both), so equality is "same id AND same literal".
fn same_object(a: &MemoryEdge, b: &MemoryEdge) -> bool {
    a.object_id == b.object_id && a.object_literal == b.object_literal
}

/// True if an OPEN edge with the same (platform, project, subject, predicate,
/// object) already exists. Used to dedup re-asserted edges and cross-extractor
/// overlap — the regex and LLM passes independently extract "Alice works at
/// Acme" and would otherwise write two identical edges. Idempotent extraction.
pub async fn open_duplicate_exists<S: Storage + ?Sized>(
    storage: &S,
    edge: &MemoryEdge,
) -> Result<bool, StorageError> {
    let open = storage
        .query_edges(&EdgeFilter {
            platform_id: Some(edge.platform_id.clone()),
            project_id: edge.project_id.clone(),
            subject_id: Some(edge.subject_id.clone()),
            predicate: Some(edge.predicate.clone()),
            current_only: true,
            ..Default::default()
        })
        .await?;
    Ok(open.iter().any(|x| x.id != edge.id && same_object(x, edge)))
}

/// Temporally supersede any open edge that conflicts with `new_edge` — same
/// (platform, project, subject, predicate) but a different object — by closing
/// it at `new_edge.valid_from`. Call this for FUNCTIONAL (single-valued)
/// predicates only (`livesIn`, `worksAt`, `status`), never multi-valued ones
/// (`likes`, `tagged`) where several objects legitimately coexist.
///
/// Idempotent: re-asserting the same object closes nothing. Returns the ids of
/// the edges it closed (empty when there was no conflict). Does NOT save
/// `new_edge` — the caller owns that, so this composes with any write path.
pub async fn supersede_conflicting_edges<S: Storage + ?Sized>(
    storage: &S,
    new_edge: &MemoryEdge,
) -> Result<Vec<String>, StorageError> {
    let open = storage
        .query_edges(&EdgeFilter {
            platform_id: Some(new_edge.platform_id.clone()),
            project_id: new_edge.project_id.clone(),
            subject_id: Some(new_edge.subject_id.clone()),
            predicate: Some(new_edge.predicate.clone()),
            current_only: true,
            ..Default::default()
        })
        .await?;

    let mut closed = Vec::new();
    for e in open {
        // Don't close the new edge against itself, and leave a re-assertion
        // of the same fact alone (idempotent).
        if e.id == new_edge.id || same_object(&e, new_edge) {
            continue;
        }
        storage.invalidate_edge(&e.id, new_edge.valid_from).await?;
        closed.push(e.id);
    }
    Ok(closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStore;
    use chrono::{Duration, Utc};

    #[test]
    fn functional_predicate_default_set() {
        // Conservative default: single-valued relations supersede; multi-valued
        // ones (many objects legitimately coexist) never do.
        std::env::remove_var("MEMORY_TEMPORAL_FUNCTIONAL_PREDICATES");
        assert!(is_functional_predicate("works_at"));
        assert!(is_functional_predicate("lives_in"));
        assert!(!is_functional_predicate("invested_in"));
        assert!(!is_functional_predicate("attended"));
        assert!(!is_functional_predicate("likes"));
    }
    use memory_core::graph::MemoryEntity;
    use memory_core::MemoryScope;

    fn entity(id: &str) -> MemoryEntity {
        let now = Utc::now();
        MemoryEntity {
            id: id.to_string(),
            created: now,
            updated: now,
            platform_id: "p".into(),
            project_id: Some("proj".into()),
            location_id: None,
            chatbot_id: None,
            chat_identity_id: None,
            scope: MemoryScope::Project,
            type_: "subject".into(),
            canonical_name: id.to_string(),
            aliases: vec![],
            description: None,
            metadata: serde_json::Value::Null,
            valid_from: now,
            valid_to: None,
            superseded_by_id: None,
        }
    }

    fn literal_edge(id: &str, subject: &str, predicate: &str, object: &str) -> MemoryEdge {
        let now = Utc::now();
        MemoryEdge {
            id: id.to_string(),
            created: now,
            updated: now,
            platform_id: "p".into(),
            project_id: Some("proj".into()),
            location_id: None,
            chatbot_id: None,
            chat_identity_id: None,
            scope: MemoryScope::Project,
            subject_id: subject.into(),
            predicate: predicate.into(),
            object_id: None,
            object_literal: Some(object.into()),
            weight: 1.0,
            source_memory_id: None,
            metadata: serde_json::Value::Null,
            valid_from: now,
            valid_to: None,
        }
    }

    async fn store() -> SqliteStore {
        let s = SqliteStore::connect("sqlite::memory:").await.unwrap();
        s.migrate().await.unwrap();
        s
    }

    /// Seed the subject entity an edge's FK requires.
    async fn seed_subject(s: &SqliteStore, id: &str) {
        s.save_entity(&entity(id)).await.unwrap();
    }

    #[tokio::test]
    async fn supersedes_on_changed_object() {
        let s = store().await;
        seed_subject(&s, "alice").await;
        let mut old = literal_edge("e-old", "alice", "livesIn", "Boston");
        old.valid_from = Utc::now() - Duration::days(10);
        s.save_edge(&old).await.unwrap();

        let new = literal_edge("e-new", "alice", "livesIn", "Denver");
        let closed = supersede_conflicting_edges(&s, &new).await.unwrap();
        assert_eq!(closed, vec!["e-old".to_string()]);
        s.save_edge(&new).await.unwrap();

        // Exactly one current edge, and it's the new one.
        let current = s
            .query_edges(&EdgeFilter {
                subject_id: Some("alice".into()),
                predicate: Some("livesIn".into()),
                current_only: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].id, "e-new");
    }

    #[tokio::test]
    async fn open_duplicate_detected() {
        let s = store().await;
        seed_subject(&s, "alice").await;
        let e = literal_edge("e1", "alice", "livesIn", "Boston");
        s.save_edge(&e).await.unwrap();
        // Same subject+predicate+object, different id → duplicate.
        let dup = literal_edge("e2", "alice", "livesIn", "Boston");
        assert!(open_duplicate_exists(&s, &dup).await.unwrap());
        // Different object → not a duplicate.
        let diff = literal_edge("e3", "alice", "livesIn", "Denver");
        assert!(!open_duplicate_exists(&s, &diff).await.unwrap());
    }

    #[tokio::test]
    async fn point_in_time_returns_past_belief() {
        let s = store().await;
        seed_subject(&s, "alice").await;
        let t0 = Utc::now() - Duration::days(10);
        let t_switch = Utc::now() - Duration::days(3);

        let mut old = literal_edge("e-old", "alice", "livesIn", "Boston");
        old.valid_from = t0;
        s.save_edge(&old).await.unwrap();

        let mut new = literal_edge("e-new", "alice", "livesIn", "Denver");
        new.valid_from = t_switch;
        supersede_conflicting_edges(&s, &new).await.unwrap();
        s.save_edge(&new).await.unwrap();

        // As of 5 days ago (before the switch): we believed Boston.
        let past = s
            .query_edges(&EdgeFilter {
                subject_id: Some("alice".into()),
                predicate: Some("livesIn".into()),
                as_of: Some(Utc::now() - Duration::days(5)),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(past.len(), 1);
        assert_eq!(past[0].id, "e-old");

        // As of now: Denver.
        let present = s
            .query_edges(&EdgeFilter {
                subject_id: Some("alice".into()),
                predicate: Some("livesIn".into()),
                as_of: Some(Utc::now()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(present.len(), 1);
        assert_eq!(present[0].id, "e-new");
    }

    #[tokio::test]
    async fn idempotent_on_same_object() {
        let s = store().await;
        seed_subject(&s, "acme").await;
        let e = literal_edge("e1", "acme", "status", "active");
        s.save_edge(&e).await.unwrap();
        // Re-asserting the same (subject, predicate, object) closes nothing.
        let again = literal_edge("e2", "acme", "status", "active");
        let closed = supersede_conflicting_edges(&s, &again).await.unwrap();
        assert!(closed.is_empty());
    }
}
