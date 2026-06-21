// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Exchange a long-lived license key (`tflk_...`) for a short-lived
//! signed JWT at the ThinkFleet SaaS.
//!
//! Why two-stage?
//!
//! License keys are the *persistent* credential customers hold — they
//! survive engine restarts, subscription renewals, and OS upgrades.
//! JWTs are *runtime* credentials with a 1-hour TTL; the engine
//! verifies them locally without phoning home on every save.
//!
//! Boot flow:
//!
//!   1. Engine loads `tflk_...` from MEMORY_LICENSE_TOKEN env or from
//!      `~/.memmesh/license.jwt` (file name is legacy; it can
//!      hold either a JWT or a license key, distinguished by prefix).
//!   2. If the loaded credential starts with `tflk_`, engine POSTs it
//!      to `POST /v1/license/validate-key`. SaaS validates the key
//!      against `license_key` rows, signs a JWT with the current
//!      subscription claims, returns it.
//!   3. Engine treats the returned JWT as the active license for the
//!      process lifetime. (Phase 2 will add periodic refresh via
//!      `/v1/license/refresh` near `refresh_at`.)
//!
//! Failure modes:
//!
//!   - SaaS unreachable → fall back to free tier with a warning. The
//!     customer can still use basic save/recall while offline; paid
//!     features are gated until the next successful exchange.
//!   - Key revoked / expired → SaaS returns 401; engine reports
//!     "license rejected" and falls back to free tier.

use serde::{Deserialize, Serialize};

/// Default exchange endpoint. Matches `licenseController` route
/// `/validate-key` under prefix `/v1/license` registered in the
/// memory-thinkfleet API server.
pub const DEFAULT_VALIDATE_KEY_ENDPOINT: &str =
    "https://api.memmesh.ai/v1/license/validate-key";

/// Tokens with this prefix are SaaS-issued license keys that need
/// exchanging for a JWT. Tokens without the prefix are assumed to be
/// JWTs and verified locally.
pub const LICENSE_KEY_PREFIX: &str = "tflk_";

const EXCHANGE_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Serialize)]
struct ValidateKeyRequest {
    #[serde(rename = "licenseKey")]
    license_key: String,
}

/// Response body the SaaS returns on a successful exchange. Mirrors
/// the shape in `packages/server/api/src/app/license/license-controller.ts`.
#[derive(Debug, Deserialize)]
pub struct ValidateKeyResponse {
    pub token: String,
    #[serde(default)]
    pub plan_tier: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub memory_cap: Option<u64>,
    #[serde(default)]
    pub features: Option<Vec<String>>,
    #[serde(default, rename = "nextRefreshAt")]
    pub next_refresh_at: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ValidateKeyError {
    #[error("could not reach license server at {endpoint}: {source}")]
    Network {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("license key rejected ({status}): {message}")]
    Rejected { status: u16, message: String },
    #[error("license server returned invalid response: {0}")]
    BadResponse(String),
}

/// Resolve which endpoint to call. CLI flag wins over env var wins
/// over the compiled-in default.
pub fn resolve_endpoint(override_url: Option<&str>) -> String {
    override_url
        .map(str::to_string)
        .or_else(|| std::env::var("THINKFLEET_VALIDATE_KEY_ENDPOINT").ok())
        .unwrap_or_else(|| DEFAULT_VALIDATE_KEY_ENDPOINT.to_string())
}

/// True if this credential is a `tflk_` license key (needs exchange).
/// False means it's a JWT that can be verified locally.
pub fn looks_like_license_key(token: &str) -> bool {
    token.starts_with(LICENSE_KEY_PREFIX)
}

/// Exchange a `tflk_...` license key for a signed JWT. Returns the
/// raw JWT string; caller is responsible for parsing it (typically
/// via `License::from_token`).
pub async fn exchange(
    license_key: &str,
    endpoint: Option<&str>,
) -> Result<ValidateKeyResponse, ValidateKeyError> {
    let url = resolve_endpoint(endpoint);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(EXCHANGE_TIMEOUT_SECS))
        .build()
        .map_err(|e| ValidateKeyError::Network {
            endpoint: url.clone(),
            source: e,
        })?;

    let resp = client
        .post(&url)
        .json(&ValidateKeyRequest {
            license_key: license_key.trim().to_string(),
        })
        .send()
        .await
        .map_err(|e| ValidateKeyError::Network {
            endpoint: url.clone(),
            source: e,
        })?;

    let status = resp.status();
    if !status.is_success() {
        let message = resp
            .text()
            .await
            .ok()
            .and_then(|body| {
                serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| {
                        v.get("message")
                            .or_else(|| v.get("error"))
                            .and_then(|x| x.as_str().map(str::to_string))
                    })
                    .or(Some(body))
            })
            .unwrap_or_else(|| "no body".to_string());
        return Err(ValidateKeyError::Rejected {
            status: status.as_u16(),
            message,
        });
    }

    resp.json::<ValidateKeyResponse>()
        .await
        .map_err(|e| ValidateKeyError::BadResponse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_license_key_prefix() {
        assert!(looks_like_license_key("tflk_abc123"));
        assert!(!looks_like_license_key("eyJhbGciOi.."));
        assert!(!looks_like_license_key(""));
        assert!(!looks_like_license_key("TFLK_uppercase"));
    }

    #[test]
    fn resolve_endpoint_prefers_override() {
        let r = resolve_endpoint(Some("http://override.test/v1/license/validate-key"));
        assert_eq!(r, "http://override.test/v1/license/validate-key");
    }

    #[test]
    fn resolve_endpoint_falls_back_to_default() {
        // Clear env to make this deterministic regardless of host env.
        std::env::remove_var("THINKFLEET_VALIDATE_KEY_ENDPOINT");
        let r = resolve_endpoint(None);
        assert_eq!(r, DEFAULT_VALIDATE_KEY_ENDPOINT);
    }
}
