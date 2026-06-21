// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Entity identity resolution — the missing primitive that lets every
//! mention of "Kevin" / "Kevin Smith" / "kevin@gotolig.com" resolve to
//! the same canonical `MemoryEntity` row instead of creating split-brain
//! duplicates.
//!
//! Two operations:
//!
//!   - `resolve_or_create_entity`: given a name (alias OR canonical),
//!     return the matching entity if one exists in the scope, otherwise
//!     create a new one with the given context.
//!   - `add_alias`: register a new alias for an existing entity (the
//!     resolver doesn't auto-add aliases — callers do so when they
//!     discover a new name variant; deciding when to add is policy).
//!
//! Backed by the existing `memory_entity` table + the new `alias` filter
//! on `EntityFilter` (matches against `canonical_name` and any entry in
//! the `aliases` array). Pure orchestration over the Storage trait — no
//! new schema, no new tables.
//!
//! Used by the self-wiring graph extractor in `memory_storage::observe`:
//! every observed text gets scanned for entity mentions, each mention
//! resolves to (or creates) an entity row, and typed edges are written
//! between them.

use chrono::Utc;
use memory_core::{MemoryEntity, MemoryScope};
use uuid::Uuid;

use crate::{EntityFilter, Storage, StorageError};

/// Context for `resolve_or_create_entity` — the scope and identity
/// metadata to attach to a newly-created entity. Existing matches keep
/// their original scope; only the create path uses these.
#[derive(Debug, Clone)]
pub struct EntityContext {
    pub platform_id: String,
    pub project_id: Option<String>,
    pub scope: MemoryScope,
    /// Type to assign on create: `"person"`, `"org"`, `"concept"`, etc.
    /// Resolver doesn't validate — callers know what they're extracting.
    pub default_type: String,
}

/// Find an entity that already knows `name` (as canonical name or
/// alias), within the given platform / project scope. If none exists,
/// insert a new row with `default_type` and return it.
///
/// "Within scope" semantics:
///   - Always filtered by `platform_id`.
///   - Filtered by `project_id` IF the context supplies one. A `None`
///     project_id means "any project on the platform" (useful for
///     platform-level entities like "Linus Torvalds").
///   - `current_only: true` — superseded entities are ignored on lookup
///     so a renamed entity doesn't shadow its successor.
pub async fn resolve_or_create_entity<S: Storage>(
    storage: &S,
    name: &str,
    ctx: &EntityContext,
) -> Result<MemoryEntity, StorageError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(StorageError::Other(anyhow::anyhow!(
            "entity name must be non-empty"
        )));
    }

    let filter = EntityFilter {
        platform_id: Some(ctx.platform_id.clone()),
        project_id: ctx.project_id.clone(),
        alias: Some(trimmed.to_string()),
        current_only: true,
        limit: Some(1),
        ..Default::default()
    };
    if let Some(existing) = storage.query_entities(&filter).await?.into_iter().next() {
        return Ok(existing);
    }

    let now = Utc::now();
    let entity = MemoryEntity {
        id: short_id(),
        created: now,
        updated: now,
        platform_id: ctx.platform_id.clone(),
        project_id: ctx.project_id.clone(),
        location_id: None,
        chatbot_id: None,
        chat_identity_id: None,
        scope: ctx.scope,
        type_: ctx.default_type.clone(),
        canonical_name: trimmed.to_string(),
        aliases: Vec::new(),
        description: None,
        metadata: serde_json::Value::Null,
        valid_from: now,
        valid_to: None,
        superseded_by_id: None,
    };
    storage.save_entity(&entity).await?;
    Ok(entity)
}

/// Append `alias` to the entity's alias list. No-op if the alias is
/// already known (matches canonical name OR an existing alias). Bumps
/// `updated` only on actual change.
pub async fn add_alias<S: Storage>(
    storage: &S,
    entity_id: &str,
    alias: &str,
) -> Result<(), StorageError> {
    let trimmed = alias.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let Some(mut entity) = storage.get_entity(entity_id).await? else {
        return Err(StorageError::NotFound(format!("entity {entity_id}")));
    };
    if entity.canonical_name == trimmed || entity.aliases.iter().any(|a| a == trimmed) {
        return Ok(());
    }
    entity.aliases.push(trimmed.to_string());
    entity.updated = Utc::now();
    storage.save_entity(&entity).await?;
    Ok(())
}

fn short_id() -> String {
    let raw = Uuid::now_v7().simple().to_string();
    raw.chars().take(21).collect()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStore;

    async fn make_store() -> SqliteStore {
        let s = SqliteStore::connect("sqlite::memory:").await.unwrap();
        s.migrate().await.unwrap();
        s
    }

    fn ctx() -> EntityContext {
        EntityContext {
            platform_id: "p1".into(),
            project_id: Some("proj_a".into()),
            scope: MemoryScope::Project,
            default_type: "person".into(),
        }
    }

    #[tokio::test]
    async fn resolves_existing_by_canonical_name() {
        let s = make_store().await;
        let first = resolve_or_create_entity(&s, "Kevin Smith", &ctx()).await.unwrap();
        let second = resolve_or_create_entity(&s, "Kevin Smith", &ctx()).await.unwrap();
        assert_eq!(first.id, second.id);
    }

    #[tokio::test]
    async fn resolves_existing_by_alias() {
        let s = make_store().await;
        let kevin = resolve_or_create_entity(&s, "Kevin Smith", &ctx()).await.unwrap();
        add_alias(&s, &kevin.id, "kevin@gotolig.com").await.unwrap();

        let by_email = resolve_or_create_entity(&s, "kevin@gotolig.com", &ctx())
            .await
            .unwrap();
        assert_eq!(by_email.id, kevin.id);
    }

    #[tokio::test]
    async fn creates_new_when_not_found() {
        let s = make_store().await;
        let melissa = resolve_or_create_entity(&s, "Melissa", &ctx()).await.unwrap();
        assert_eq!(melissa.canonical_name, "Melissa");
        assert!(melissa.aliases.is_empty());
    }

    #[tokio::test]
    async fn add_alias_dedupes() {
        let s = make_store().await;
        let kevin = resolve_or_create_entity(&s, "Kevin Smith", &ctx()).await.unwrap();
        add_alias(&s, &kevin.id, "K. Smith").await.unwrap();
        add_alias(&s, &kevin.id, "K. Smith").await.unwrap(); // duplicate

        let refreshed = s.get_entity(&kevin.id).await.unwrap().unwrap();
        assert_eq!(refreshed.aliases, vec!["K. Smith".to_string()]);
    }

    #[tokio::test]
    async fn add_alias_skips_canonical_name() {
        let s = make_store().await;
        let kevin = resolve_or_create_entity(&s, "Kevin Smith", &ctx()).await.unwrap();
        add_alias(&s, &kevin.id, "Kevin Smith").await.unwrap();

        let refreshed = s.get_entity(&kevin.id).await.unwrap().unwrap();
        assert!(refreshed.aliases.is_empty());
    }
}
