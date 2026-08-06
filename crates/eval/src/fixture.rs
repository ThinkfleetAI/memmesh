// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! YAML fixture loader.
//!
//! Example fixture:
//!
//! ```yaml
//! name: basic-substring
//! description: Plain substring match within a project scope
//! corpus:
//!   - id: m1
//!     type: rule
//!     content: "Team uses Rust for the memory engine"
//!     scope: project
//!     project_id: test
//!   - id: m2
//!     type: preference
//!     content: "User prefers Vitest over Jest for testing"
//!     scope: user
//! queries:
//!   - q: "Rust"
//!     project_id: test
//!     expected: [m1]
//!   - q: "Vitest"
//!     expected: [m2]
//! ```
//!
//! Fields not listed are filled with sensible defaults (importance 5.0,
//! confidence 1.0, fresh timestamps, etc.) so fixtures stay short.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Fixture {
    /// Stable name shown in scorecards. Keep kebab-case.
    pub name: String,
    /// One-sentence description shown in scorecards next to the name.
    pub description: String,
    /// Seed corpus for this fixture. Inserted in order; ids must be
    /// unique within the fixture.
    pub corpus: Vec<FixtureCorpusItem>,
    /// Queries to run after the corpus is seeded.
    pub queries: Vec<FixtureQuery>,
    /// K used for P@K / R@K. Defaults to 5.
    #[serde(default = "default_k")]
    pub k: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FixtureCorpusItem {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub content: String,
    /// Scope as a string: "platform" | "project" | "location" | "agent"
    /// | "user" | "session". Defaults to "project".
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default = "default_platform")]
    pub platform_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default = "default_importance")]
    pub importance: f32,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FixtureQuery {
    /// Substring or natural-language query string.
    pub q: String,
    /// Memory ids that SHOULD appear in the top-K result set. Order
    /// doesn't matter for P@K / R@K, but the first match's rank drives
    /// MRR.
    pub expected: Vec<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

fn default_k() -> usize {
    5
}
fn default_scope() -> String {
    "project".to_string()
}
fn default_platform() -> String {
    "eval-platform".to_string()
}
fn default_importance() -> f32 {
    5.0
}
fn default_confidence() -> f32 {
    1.0
}

impl Fixture {
    /// Load a single fixture from a YAML file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read fixture {}", path.display()))?;
        let fixture: Fixture = serde_yaml::from_str(&text)
            .with_context(|| format!("parse fixture {}", path.display()))?;
        Ok(fixture)
    }

    /// Load every `*.yaml` / `*.yml` file in a directory.
    pub fn load_dir(dir: &Path) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("read dir {}", dir.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| {
                let p = e.path();
                let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
                matches!(ext, "yaml" | "yml")
            })
            .collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            out.push(Fixture::load(&entry.path())?);
        }
        Ok(out)
    }
}
