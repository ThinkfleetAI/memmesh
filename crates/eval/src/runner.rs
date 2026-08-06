// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Wire a fixture into a fresh in-memory `SqliteStore`, run every query
//! through `Storage::query`, score the ranked result list, and return a
//! `Scorecard`.
//!
//! In-memory SQLite (`sqlite::memory:`) lets each fixture run start from
//! a clean schema with no on-disk side effects. Tests, CI, and ad-hoc
//! eval invocations all share this isolation guarantee.

use anyhow::{anyhow, Result};
use chrono::Utc;
use memory_core::{MemoryImpact, MemoryItem, MemoryScope, MemoryStatus};
use memory_storage::{sqlite::SqliteStore, MemoryFilter, MemoryQuery, Storage};
use serde::Serialize;

use crate::fixture::{Fixture, FixtureCorpusItem, FixtureQuery};
use crate::scorer::{aggregate, score_query, QueryScore, Scorecard};

#[derive(Debug, Clone, Serialize)]
pub struct SuiteResult {
    pub scorecards: Vec<Scorecard>,
    pub overall_mean_precision: f64,
    pub overall_mean_recall: f64,
    pub overall_mean_mrr: f64,
}

impl SuiteResult {
    pub fn print_human(&self) {
        let header_k = self
            .scorecards
            .first()
            .map(|s| s.k)
            .unwrap_or(5);
        println!(
            "{:<28} {:<8} {:<8} {:<8} queries",
            "fixture",
            format!("P@{header_k}"),
            format!("R@{header_k}"),
            "MRR"
        );
        println!("{}", "─".repeat(72));
        for s in &self.scorecards {
            println!("{}", s.one_line_summary());
        }
        if self.scorecards.len() > 1 {
            println!("{}", "─".repeat(72));
            println!(
                "{:<28} {:.3}    {:.3}    {:.3}    (suite mean)",
                "OVERALL",
                self.overall_mean_precision,
                self.overall_mean_recall,
                self.overall_mean_mrr,
            );
        }
    }
}

/// Run one fixture against a fresh in-memory store. Returns its
/// scorecard. Doesn't mutate any on-disk database.
pub async fn run_fixture(fixture: &Fixture) -> Result<Scorecard> {
    let store = SqliteStore::connect("sqlite::memory:")
        .await
        .map_err(|e| anyhow!("connect in-memory sqlite: {e}"))?;
    store.migrate().await.map_err(|e| anyhow!("migrate: {e}"))?;

    for item in &fixture.corpus {
        let memory = corpus_to_memory(item)?;
        store
            .save(&memory)
            .await
            .map_err(|e| anyhow!("seed item {}: {e}", item.id))?;
    }

    let mut query_scores: Vec<QueryScore> = Vec::with_capacity(fixture.queries.len());
    for q in &fixture.queries {
        let returned = run_one_query(&store, q, fixture.k * 2).await?;
        query_scores.push(score_query(&q.q, fixture.k, &returned, &q.expected));
    }

    Ok(aggregate(
        fixture.name.clone(),
        fixture.description.clone(),
        fixture.k,
        query_scores,
    ))
}

/// Run a whole directory of fixtures and roll the scores into a suite
/// summary. Used by `memmesh eval` with no args.
pub async fn run_suite(fixtures: Vec<Fixture>) -> Result<SuiteResult> {
    let mut scorecards = Vec::with_capacity(fixtures.len());
    for f in fixtures {
        scorecards.push(run_fixture(&f).await?);
    }
    let n = scorecards.len() as f64;
    let (p, r, m) = if scorecards.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        scorecards.iter().fold((0.0, 0.0, 0.0), |(p, r, m), s| {
            (p + s.mean_precision_at_k, r + s.mean_recall_at_k, m + s.mean_mrr)
        })
    };
    Ok(SuiteResult {
        scorecards,
        overall_mean_precision: if n == 0.0 { 0.0 } else { p / n },
        overall_mean_recall: if n == 0.0 { 0.0 } else { r / n },
        overall_mean_mrr: if n == 0.0 { 0.0 } else { m / n },
    })
}

async fn run_one_query(store: &SqliteStore, q: &FixtureQuery, limit: usize) -> Result<Vec<String>> {
    let scope = q
        .scope
        .as_deref()
        .map(parse_scope)
        .transpose()
        .map_err(|e| anyhow!("query scope: {e}"))?;
    let filter = MemoryFilter {
        text_match: Some(q.q.clone()),
        project_id: q.project_id.clone(),
        user_id: q.user_id.clone(),
        scope,
        ..Default::default()
    };
    let mq = MemoryQuery {
        filter,
        limit: Some(limit as u32),
        offset: None,
    };
    let items = store.query(&mq).await.map_err(|e| anyhow!("query: {e}"))?;
    Ok(items.into_iter().map(|i| i.id).collect())
}

fn corpus_to_memory(item: &FixtureCorpusItem) -> Result<MemoryItem> {
    let now = Utc::now();
    let scope = parse_scope(&item.scope).map_err(|e| anyhow!("corpus item scope: {e}"))?;
    Ok(MemoryItem {
        id: item.id.clone(),
        created: now,
        updated: now,
        platform_id: item.platform_id.clone(),
        project_id: item.project_id.clone(),
        location_id: None,
        chatbot_id: None,
        chat_identity_id: item.user_id.clone(),
        session_key: None,
        type_: item.type_.clone(),
        content: item.content.clone(),
        category: None,
        importance: item.importance,
        source: Some("eval-fixture".to_string()),
        metadata: serde_json::Value::Null,
        scope,
        status: MemoryStatus::Confirmed,
        confidence: item.confidence,
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
}

fn parse_scope(s: &str) -> Result<MemoryScope, String> {
    match s.to_ascii_lowercase().as_str() {
        "platform" => Ok(MemoryScope::Platform),
        "project" => Ok(MemoryScope::Project),
        "location" => Ok(MemoryScope::Location),
        "agent" => Ok(MemoryScope::Agent),
        "user" => Ok(MemoryScope::User),
        "session" => Ok(MemoryScope::Session),
        other => Err(format!("unknown scope `{other}`")),
    }
}
