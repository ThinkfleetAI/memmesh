// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Engine configuration. Loaded from `$THINKFLEET_MEMORY_CONFIG` or
//! `~/.memmesh/config.toml`. Missing file is fine — the engine
//! defaults to local-only mode (no sync, free-tier cap).
//!
//! Env-var overrides (set by the desktop bridge or a headless installer):
//!   THINKFLEET_SYNC_URL          → `[sync].url`
//!   THINKFLEET_SYNC_TOKEN        → `[sync].token`
//!   THINKFLEET_PLATFORM_ID       → `[sync].platform_id`
//!   THINKFLEET_FREE_TIER_CAP     → `[free_tier].entry_cap`
//!
//! Mode detection: `[sync]` section with url + token + platform_id all set
//! → SaaS-connected mode. Anything else → local-only.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default entry cap for the local engine. `0` (or any non-positive value)
/// means **unlimited** — the open-source engine imposes no ceiling on
/// `clawdbot_memory_item` row count. A positive value re-enables a hard cap
/// and is intended for commercial embedders (e.g. desktop plan tiers) that
/// set it explicitly via config or `THINKFLEET_FREE_TIER_CAP`.
pub const DEFAULT_FREE_TIER_CAP: i64 = 0;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Present iff the engine should run in SaaS-connected mode. Absence
    /// means local-only — cap enforced, no sync.
    pub sync: Option<SyncConfig>,
    /// Cap config. Defaults applied if missing.
    #[serde(default)]
    pub free_tier: FreeTierConfig,
    /// Embedding provider for semantic search. Defaults to `Local` (the small
    /// in-process `bge-small-en-v1.5` model) so the open-source `memmesh` build
    /// has real semantic search on out of the box. Override the `[embeddings]`
    /// section (or the `THINKFLEET_EMBEDDINGS_*` env vars) to pick a different
    /// model, a remote provider, or turn it off (`provider = "none"`).
    ///
    /// TIERING NOTE: the heavier `bge-large-en-v1.5` model + the ONNX runtime
    /// have no business in the lightweight desktop app. The desktop build ships
    /// *without* the `fastembed` feature, so any `[embeddings] provider =
    /// "local"` config there (including this default) degrades cleanly to
    /// `None` — lexical + recency ranking, never a failure to start. Desktop
    /// semantic search, if wanted, comes via a `Remote` provider pointing at
    /// the user's SaaS/on-prem engine.
    #[serde(default)]
    pub embeddings: memory_embed::EmbeddingConfig,
    /// Which storage backend the engine binds to at startup. Defaults to
    /// SQLite (local file). Set `backend = "postgres"` + a `url` to run
    /// against a Postgres instance instead. Changing this takes effect the
    /// next time each `memmesh` process starts (console, `serve`, and every
    /// MCP server an AI tool spawns).
    #[serde(default)]
    pub database: DatabaseConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// `sqlite` (default) or `postgres`.
    #[serde(default)]
    pub backend: DatabaseBackend,
    /// Postgres connection URL (`postgres://user:pass@host:5432/dbname`).
    /// Required when `backend = "postgres"`; ignored for sqlite, which uses
    /// the `--db` path / `THINKFLEET_MEMORY_DB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseBackend {
    #[default]
    Sqlite,
    Postgres,
}

impl DatabaseBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            DatabaseBackend::Sqlite => "sqlite",
            DatabaseBackend::Postgres => "postgres",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// SaaS base URL, e.g. `https://api.memmesh.ai`. Required.
    pub url: String,
    /// Bearer token. Desktop writes this after login. Required.
    pub token: String,
    /// SaaS platform id for the signed-in user. Required.
    pub platform_id: String,
    /// Sync interval in seconds. Defaults to 30. The `serve` daemon uses
    /// this for the background push/pull loop.
    #[serde(default = "default_sync_interval")]
    pub interval_seconds: u64,
    /// How to handle an unbound cwd in SaaS mode:
    /// `"auto_create"` (default) → engine asks SaaS to create a project.
    /// `"soft_fail"`             → save locally, skip push, warn.
    /// `"hard_fail"`             → refuse the write entirely.
    #[serde(default = "default_binding_policy")]
    pub binding_policy: BindingPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingPolicy {
    AutoCreate,
    SoftFail,
    HardFail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreeTierConfig {
    /// Max `clawdbot_memory_item` rows the engine will accept in local-only
    /// mode. `0` (the default) = unlimited; a positive value enables a hard
    /// cap for commercial embedders.
    #[serde(default = "default_free_tier_cap")]
    pub entry_cap: i64,
}

impl Default for FreeTierConfig {
    fn default() -> Self {
        Self {
            entry_cap: DEFAULT_FREE_TIER_CAP,
        }
    }
}

fn default_sync_interval() -> u64 {
    30
}
fn default_binding_policy() -> BindingPolicy {
    BindingPolicy::AutoCreate
}
fn default_free_tier_cap() -> i64 {
    DEFAULT_FREE_TIER_CAP
}

impl Config {
    /// Load from `$THINKFLEET_MEMORY_CONFIG` or
    /// `~/.memmesh/config.toml`. Missing file → defaults
    /// (local-only mode, free-tier cap). Env vars override the TOML.
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::resolve_path();
        let mut config = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
            toml::from_str::<Config>(&text)
                .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?
        } else {
            Config::default()
        };
        config.apply_env_overrides();
        Ok(config)
    }

    /// Same as `load` but never errors — returns defaults on any failure.
    /// Used in hot paths (MCP, observe) where a broken config shouldn't
    /// kill the write path.
    pub fn load_or_default() -> Self {
        Self::load().unwrap_or_default()
    }

    /// Where the config file should live. Honors
    /// `$THINKFLEET_MEMORY_CONFIG`; otherwise `~/.memmesh/config.toml`.
    pub fn resolve_path() -> PathBuf {
        if let Ok(p) = std::env::var("THINKFLEET_MEMORY_CONFIG") {
            return PathBuf::from(p);
        }
        let home = std::env::var_os("HOME").unwrap_or_default();
        PathBuf::from(home)
            .join(".memmesh")
            .join("config.toml")
    }

    /// Directory where the engine writes rotating log files. `~/.memmesh/logs`
    /// unless `THINKFLEET_MEMORY_LOG_DIR` overrides it. Every `memmesh`
    /// process (mcp / serve / console) appends to the same file here, so the
    /// web console can tail a single unified log across all engine activity.
    pub fn log_dir() -> PathBuf {
        if let Ok(p) = std::env::var("THINKFLEET_MEMORY_LOG_DIR") {
            return PathBuf::from(p);
        }
        let home = std::env::var_os("HOME").unwrap_or_default();
        PathBuf::from(home).join(".memmesh").join("logs")
    }

    /// The unified engine log file (`<log_dir>/memmesh.log`).
    pub fn log_file() -> PathBuf {
        Self::log_dir().join("memmesh.log")
    }

    /// True iff sync section is fully populated. The engine treats this as
    /// "attempt SaaS-connected mode" — the token still has to validate
    /// successfully on its first sync attempt before paid features unlock.
    pub fn is_saas_configured(&self) -> bool {
        self.sync
            .as_ref()
            .map(|s| !s.url.is_empty() && !s.token.is_empty() && !s.platform_id.is_empty())
            .unwrap_or(false)
    }

    /// Persist back to the config path. Used by the `config` CLI
    /// subcommand and by the desktop bridge after login.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::resolve_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("mkdir {}: {e}", parent.display()))?;
        }
        let text =
            toml::to_string_pretty(self).map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
        std::fs::write(&path, text)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
        Ok(())
    }

    fn apply_env_overrides(&mut self) {
        let sync = self.sync.get_or_insert_with(|| SyncConfig {
            url: String::new(),
            token: String::new(),
            platform_id: String::new(),
            interval_seconds: default_sync_interval(),
            binding_policy: default_binding_policy(),
        });

        if let Ok(v) = std::env::var("THINKFLEET_SYNC_URL") {
            if !v.is_empty() {
                sync.url = v;
            }
        }
        if let Ok(v) = std::env::var("THINKFLEET_SYNC_TOKEN") {
            if !v.is_empty() {
                sync.token = v;
            }
        }
        if let Ok(v) = std::env::var("THINKFLEET_PLATFORM_ID") {
            if !v.is_empty() {
                sync.platform_id = v;
            }
        }

        // If env didn't populate anything and the on-disk file had no
        // [sync] section, drop the empty struct we just created so
        // is_saas_configured() returns false cleanly.
        if sync.url.is_empty() && sync.token.is_empty() && sync.platform_id.is_empty() {
            self.sync = None;
        }

        if let Ok(v) = std::env::var("THINKFLEET_FREE_TIER_CAP") {
            if let Ok(n) = v.parse::<i64>() {
                self.free_tier.entry_cap = n;
            }
        }

        // Embeddings provider via env (k8s-friendly — the pod sets these instead
        // of mounting a TOML). THINKFLEET_EMBEDDINGS_PROVIDER ∈
        // none|hashed|local|remote; _URL (remote), _MODEL (local/remote), _DIM
        // (hashed/remote). Unknown provider leaves config unchanged.
        if let Ok(provider) = std::env::var("THINKFLEET_EMBEDDINGS_PROVIDER") {
            let model = std::env::var("THINKFLEET_EMBEDDINGS_MODEL").ok().filter(|s| !s.is_empty());
            let dim =
                std::env::var("THINKFLEET_EMBEDDINGS_DIM").ok().and_then(|s| s.parse::<usize>().ok());
            let cfg = match provider.as_str() {
                "none" => Some(memory_embed::EmbeddingConfig::None),
                "hashed" => Some(memory_embed::EmbeddingConfig::Hashed { dim: dim.unwrap_or(256) }),
                "local" => Some(memory_embed::EmbeddingConfig::Local { model }),
                "remote" => Some(memory_embed::EmbeddingConfig::Remote {
                    url: std::env::var("THINKFLEET_EMBEDDINGS_URL").unwrap_or_default(),
                    model,
                    dim: dim.unwrap_or(1024),
                }),
                other => {
                    tracing::warn!(provider = %other, "unknown THINKFLEET_EMBEDDINGS_PROVIDER; ignoring");
                    None
                }
            };
            if let Some(c) = cfg {
                self.embeddings = c;
            }
        }

        // Backend selection via env. `THINKFLEET_DATABASE_URL` (or the sqlx
        // convention `DATABASE_URL`) switches the engine to Postgres and
        // supplies the DSN — handy for CI, containers, and one-off overrides
        // without editing the TOML.
        if let Ok(url) = std::env::var("THINKFLEET_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL")) {
            if !url.is_empty() {
                self.database.backend = DatabaseBackend::Postgres;
                self.database.url = Some(url);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_local_only() {
        let c = Config::default();
        assert!(!c.is_saas_configured());
        assert_eq!(c.free_tier.entry_cap, DEFAULT_FREE_TIER_CAP);
    }

    #[test]
    fn fully_populated_sync_is_saas_configured() {
        let c = Config {
            sync: Some(SyncConfig {
                url: "https://example.com".into(),
                token: "t".into(),
                platform_id: "plat".into(),
                interval_seconds: 30,
                binding_policy: BindingPolicy::AutoCreate,
            }),
            free_tier: FreeTierConfig::default(),
            embeddings: Default::default(),
        };
        assert!(c.is_saas_configured());
    }

    #[test]
    fn partial_sync_is_not_saas_configured() {
        let c = Config {
            sync: Some(SyncConfig {
                url: "https://example.com".into(),
                token: "".into(),
                platform_id: "plat".into(),
                interval_seconds: 30,
                binding_policy: BindingPolicy::AutoCreate,
            }),
            free_tier: FreeTierConfig::default(),
            embeddings: Default::default(),
        };
        assert!(!c.is_saas_configured());
    }

    #[test]
    fn embeddings_defaults_to_local_and_parses_explicit() {
        // No [embeddings] section → Local (semantic on by default in the
        // open-source build; degrades to None when built without fastembed).
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(
            c.embeddings,
            memory_embed::EmbeddingConfig::Local { model: None }
        );

        // An explicit local-provider section round-trips.
        let c: Config = toml::from_str(
            "[embeddings]\nprovider = \"local\"\nmodel = \"bge-large-en-v1.5\"\n",
        )
        .unwrap();
        assert_eq!(
            c.embeddings,
            memory_embed::EmbeddingConfig::Local { model: Some("bge-large-en-v1.5".into()) }
        );

        // Semantic search can still be turned off explicitly.
        let c: Config = toml::from_str("[embeddings]\nprovider = \"none\"\n").unwrap();
        assert_eq!(c.embeddings, memory_embed::EmbeddingConfig::None);
    }

    #[test]
    fn embeddings_env_override_sets_remote() {
        std::env::set_var("THINKFLEET_EMBEDDINGS_PROVIDER", "remote");
        std::env::set_var("THINKFLEET_EMBEDDINGS_URL", "http://embed:8080");
        std::env::set_var("THINKFLEET_EMBEDDINGS_MODEL", "bge-large-en-v1.5");
        std::env::set_var("THINKFLEET_EMBEDDINGS_DIM", "1024");

        let mut c = Config::default();
        c.apply_env_overrides();
        assert_eq!(
            c.embeddings,
            memory_embed::EmbeddingConfig::Remote {
                url: "http://embed:8080".into(),
                model: Some("bge-large-en-v1.5".into()),
                dim: 1024,
            }
        );

        for k in [
            "THINKFLEET_EMBEDDINGS_PROVIDER",
            "THINKFLEET_EMBEDDINGS_URL",
            "THINKFLEET_EMBEDDINGS_MODEL",
            "THINKFLEET_EMBEDDINGS_DIM",
        ] {
            std::env::remove_var(k);
        }
    }
}
