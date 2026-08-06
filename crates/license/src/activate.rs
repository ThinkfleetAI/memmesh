// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Activation flow: redeem a purchase code at the ThinkFleet SaaS for
//! a signed license JWT bound to this machine.
//!
//! The customer journey:
//!   1. Buy a plan at api.memmesh.ai
//!   2. Receive an activation code (e.g. `TF-A1B2-C3D4-E5F6`) via email
//!   3. Run `memmesh activate TF-A1B2-C3D4-E5F6`
//!   4. Engine POSTs `{code, machine_fingerprint, machine_label}` to the
//!      SaaS, receives a JWT, writes it to `~/.memmesh/license.jwt`
//!   5. Engine restarts pick up the new license automatically because
//!      `MEMORY_LICENSE_PATH` defaults to that file when it exists.
//!
//! The SaaS validates the code against memmesh.ai internally
//! and signs the JWT with the plan's `memory_cap`, `features`, and
//! `host_binding` claim baked in. Subsequent activations from a
//! different machine fail at the host-binding check in
//! `verify_license`.

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{fingerprint, License};

/// Default activation endpoint. Override per-environment via the
/// `THINKFLEET_ACTIVATION_ENDPOINT` env var (useful for staging,
/// for local mock servers during dev, and for self-host deploys).
pub const DEFAULT_ACTIVATION_ENDPOINT: &str =
    "https://api.memmesh.ai/api/v1/license/activate";

/// HTTP timeout — generous enough to survive a slow billing-side
/// signing call, tight enough that the CLI doesn't hang forever if
/// the SaaS is unreachable.
const ACTIVATION_TIMEOUT_SECS: u64 = 30;

/// Request body the SaaS expects on `POST /api/v1/license/activate`.
#[derive(Debug, Serialize)]
struct ActivateRequest {
    code: String,
    machine_fingerprint: String,
    /// Human-readable hostname for the customer's "active devices"
    /// dashboard. Display-only — never used for validation.
    machine_label: String,
}

/// Response body the SaaS returns on a successful activation.
///
/// The token is the signed license JWT; the other fields are
/// duplicated from the JWT claims for client-side display before
/// the engine bothers to verify the signature.
#[derive(Debug, Deserialize)]
struct ActivateResponse {
    token: String,
    tier: String,
    memory_cap: u64,
    features: Vec<String>,
    expires_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ActivateError {
    #[error("could not reach activation server at {endpoint}: {source}")]
    Network {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("activation rejected ({status}): {message}")]
    Rejected { status: u16, message: String },
    #[error("activation server returned invalid response: {0}")]
    BadResponse(String),
    #[error("returned token failed verification: {0}")]
    BadToken(String),
    #[error("could not persist license to {path}: {source}")]
    Persist {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Outcome of a successful activate() call. Returned so the CLI can
/// pretty-print to the customer.
pub struct ActivateOutcome {
    pub license: License,
    pub written_to: PathBuf,
    pub tier: String,
    pub memory_cap: u64,
    pub features: Vec<String>,
    pub expires_at: String,
}

/// Default path the activate flow writes the license JWT to. Lives in
/// the engine's data dir so future engine starts auto-discover it via
/// the `MEMORY_LICENSE_PATH` default.
pub fn default_license_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".memmesh").join("license.jwt")
}

/// Redeem an activation code at the SaaS, persist the returned JWT,
/// and return the verified license.
///
/// `endpoint` falls back to the env var `THINKFLEET_ACTIVATION_ENDPOINT`,
/// then to `DEFAULT_ACTIVATION_ENDPOINT`. Lets tests point at a mock
/// server without touching the binary's source.
pub async fn activate(
    code: &str,
    endpoint: Option<&str>,
) -> Result<ActivateOutcome, ActivateError> {
    let url = endpoint
        .map(str::to_string)
        .or_else(|| std::env::var("THINKFLEET_ACTIVATION_ENDPOINT").ok())
        .unwrap_or_else(|| DEFAULT_ACTIVATION_ENDPOINT.to_string());

    let body = ActivateRequest {
        code: code.trim().to_string(),
        machine_fingerprint: fingerprint::machine_fingerprint(),
        machine_label: fingerprint::machine_label(),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(ACTIVATION_TIMEOUT_SECS))
        .build()
        .map_err(|e| ActivateError::Network {
            endpoint: url.clone(),
            source: e,
        })?;

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| ActivateError::Network {
            endpoint: url.clone(),
            source: e,
        })?;

    let status = resp.status();
    if !status.is_success() {
        // Try to read the SaaS error envelope; fall back to a generic
        // message if the body isn't valid JSON.
        let message = resp
            .text()
            .await
            .ok()
            .and_then(|body| {
                serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                    .or(Some(body))
            })
            .unwrap_or_else(|| "no body".to_string());
        return Err(ActivateError::Rejected {
            status: status.as_u16(),
            message,
        });
    }

    let parsed: ActivateResponse = resp
        .json()
        .await
        .map_err(|e| ActivateError::BadResponse(e.to_string()))?;

    // Verify before persisting. Refusing to write a bogus token saves
    // the customer a confusing "the engine still says free tier" bug
    // report.
    let license = License::from_token(&parsed.token, Utc::now());
    if !license.allows_writes() {
        return Err(ActivateError::BadToken(format!(
            "SaaS returned a token the engine refuses: {:?}",
            license.status
        )));
    }

    // Persist to disk so the engine picks it up on next start. The
    // file is 0600 (`u+rw,go-rwx`) since it's a bearer credential —
    // anyone who reads it can impersonate this license until expiry.
    let path = default_license_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ActivateError::Persist {
            path: path.display().to_string(),
            source: e,
        })?;
    }
    std::fs::write(&path, &parsed.token).map_err(|e| ActivateError::Persist {
        path: path.display().to_string(),
        source: e,
    })?;
    set_owner_only_perms(&path);

    Ok(ActivateOutcome {
        license,
        written_to: path,
        tier: parsed.tier,
        memory_cap: parsed.memory_cap,
        features: parsed.features,
        expires_at: parsed.expires_at,
    })
}

#[cfg(unix)]
fn set_owner_only_perms(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_owner_only_perms(_path: &std::path::Path) {
    // Windows ACLs are out of scope for v1; the desktop install is
    // unix-first.
}
