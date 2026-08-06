// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! License credential storage — how the engine sources its current
//! license at startup.
//!
//! Three sources, in priority order:
//!
//!   1. Env var `MEMORY_LICENSE_TOKEN` — the raw credential directly.
//!      Useful for k8s deployments and quick smoke tests.
//!
//!   2. File at `MEMORY_LICENSE_PATH` — explicit file path override.
//!
//!   3. Default file at `~/.memmesh/license.jwt` — written
//!      by the `memmesh activate` flow. Auto-discovered when
//!      the file exists so customers don't have to manually wire env
//!      vars after activation.
//!
//! Each source can hold either:
//!   - A `tflk_...` license key (persistent credential issued by the
//!     SaaS at `/platform/license`). Exchanged for a JWT at startup
//!     via `validate_key::exchange`.
//!   - A raw JWT (`eyJ...`). Verified locally against the embedded
//!     public key.
//!
//! All paths converge on a [`License`] so the rest of the engine
//! doesn't care where the credential came from. Missing/invalid →
//! free-tier defaults; the engine stays usable.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::{activate, validate_key, License};

/// Load the engine's license from the environment. See module docs
/// for source precedence. Async because `tflk_...` keys require an
/// HTTP round-trip to the SaaS to be exchanged for a JWT.
pub async fn load_from_env(now: DateTime<Utc>) -> License {
    if let Ok(token) = std::env::var("MEMORY_LICENSE_TOKEN") {
        let trimmed = token.trim().to_string();
        if !trimmed.is_empty() {
            tracing::info!("license: using MEMORY_LICENSE_TOKEN");
            return resolve_credential(&trimmed, now).await;
        }
    }
    if let Ok(path) = std::env::var("MEMORY_LICENSE_PATH") {
        if let Some(lic) = load_from_path(&PathBuf::from(&path), now, "MEMORY_LICENSE_PATH").await {
            return lic;
        }
    }
    // Auto-discovery: the `activate` subcommand writes here, so a
    // freshly-activated install Just Works without env var setup.
    let default_path = activate::default_license_path();
    if default_path.exists() {
        if let Some(lic) = load_from_path(&default_path, now, "default license file").await {
            return lic;
        }
    }
    tracing::info!("license: no credential, running free tier");
    License::free(now)
}

async fn load_from_path(path: &PathBuf, now: DateTime<Utc>, source: &str) -> Option<License> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                tracing::info!(path = %path.display(), %source, "license: loaded from file");
                return Some(resolve_credential(trimmed, now).await);
            }
            tracing::warn!(path = %path.display(), %source, "license file is empty; ignoring");
            None
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %source, %err, "license file unreadable; ignoring");
            None
        }
    }
}

/// Resolve any credential (JWT or `tflk_...` key) into a [`License`].
/// `tflk_...` keys are exchanged at the SaaS for a JWT; raw JWTs are
/// verified locally. SaaS failures fall back to the free tier with a
/// warning — the engine stays usable while the network or the SaaS is
/// down, just capped.
async fn resolve_credential(credential: &str, now: DateTime<Utc>) -> License {
    if validate_key::looks_like_license_key(credential) {
        tracing::info!("license: credential is a license key, exchanging for JWT");
        match validate_key::exchange(credential, None).await {
            Ok(resp) => {
                tracing::info!(
                    plan = ?resp.plan_tier,
                    cap = ?resp.memory_cap,
                    "license: exchanged for JWT",
                );
                return License::from_token(&resp.token, now);
            }
            Err(err) => {
                tracing::warn!(%err, "license: exchange failed; falling back to free tier");
                return License::free(now);
            }
        }
    }
    License::from_token(credential, now)
}
