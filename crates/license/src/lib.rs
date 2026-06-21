// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! License-key verification + cap enforcement for the desktop engine.
//!
//! The engine is the same Rust binary in three places: ThinkFleet's own
//! SaaS, third-party SaaS apps that embed the engine, and the local
//! desktop runtime. Billing alone is not enough — anyone with the
//! binary could bypass cap limits unless the engine itself refuses.
//!
//! This module is the refusal mechanism:
//!
//!   - [`LicenseClaims`] is a JWT-style payload signed by ThinkFleet
//!     (Ed25519). The public key is baked into the binary as a
//!     compile-time constant — tampering requires recompiling, which
//!     paid customers can't do without source.
//!
//!   - [`verify_license`] returns a [`LicenseStatus`] indicating whether
//!     the engine should serve writes (Valid / GracePeriod) or refuse
//!     them (Expired / Invalid).
//!
//!   - [`check_write_allowed`] is the hot-path enforcement point: it
//!     compares the current memory count against the cap claim and
//!     returns [`LicenseError::CapExceeded`] if over.
//!
//! Default (no token loaded) = Free tier with [`FREE_TIER_MEMORY_CAP`].
//! The engine works out of the box; paid features require a signed
//! token in `MEMORY_LICENSE_TOKEN` or at `MEMORY_LICENSE_PATH`.

use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

pub mod activate;
pub mod fingerprint;
pub mod store;
pub mod validate_key;

/// Embedded Ed25519 public verification key. PEM SubjectPublicKeyInfo
/// format. Kept in sync with the SaaS-side
/// `memory-retrieve/src/license/mod.rs` so the same SaaS-issued JWTs
/// verify on the desktop engine.
///
/// The matching private *signing* key is held OFFLINE and is never
/// committed to this repository — only this public verification key
/// ships in source. In production this public key is replaced at build
/// time via include_bytes! from a checked-in `keys/license_pubkey.pem`;
/// rotation is release-gated.
pub const LICENSE_PUBLIC_KEY_PEM: &[u8] = b"-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEASeFn7v8VS02tyi2XCaeSjzn8WUXki6ksGI8S8lclcgg=
-----END PUBLIC KEY-----";

/// How long a license can live past `expires_at` before the engine
/// starts refusing writes. Reads stay allowed indefinitely — data is
/// the customer's, but they can't grow it further.
pub const GRACE_PERIOD_SECS: i64 = 7 * 24 * 60 * 60; // 7 days

/// Free-tier cap when no license token is loaded. Matches the
/// settled local-desktop default (2026-05-18) and the existing
/// `quota::effective_cap` free-tier value.
pub const FREE_TIER_MEMORY_CAP: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseTier {
    Free,
    Pro,
    Team,
    Enterprise,
}

impl LicenseTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Pro => "pro",
            Self::Team => "team",
            Self::Enterprise => "enterprise",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseEnvironment {
    /// Desktop / local engine, single-user, OAuth-bound.
    Desktop,
    /// Third-party SaaS app embedding the engine. License key bound
    /// to a hostname or container fingerprint.
    SelfHostSaas,
    /// ThinkFleet's own SaaS. Internal service account.
    ThinkfleetSaas,
}

/// JWT claims for an engine license. Signed by ThinkFleet's
/// `license_signing_key`, verified by the embedded public key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseClaims {
    /// Billing customer id assigned by the issuing billing system.
    pub customer_id: String,
    pub plan_tier: LicenseTier,
    /// Max memory items. `u64::MAX` = unlimited.
    pub memory_cap: u64,
    /// Feature flags this license unlocks (lattice, packs, etc.). Gates
    /// which MCP tools and CLI subcommands are visible.
    #[serde(default)]
    pub features: Vec<String>,
    pub environment: LicenseEnvironment,
    /// Optional binding to a hostname or container fingerprint. Used
    /// for SelfHostSaas environments; ignored for Desktop and Thinkfleet.
    #[serde(default)]
    pub host_binding: Option<String>,
    /// Standard JWT claims.
    #[serde(rename = "exp")]
    pub expires_at: i64,
    #[serde(rename = "iat")]
    pub issued_at: i64,
    /// Recommended refresh time — the engine should ping the SaaS for
    /// a fresh JWT after this, even before `exp`. Phase 1 ignores it
    /// (no refresh task); reserved for future runtime refresh.
    pub refresh_at: i64,
    /// JWT id — unique per issued token, used for revocation lists.
    #[serde(default)]
    pub jti: Option<String>,
}

impl LicenseClaims {
    /// Default free-tier claims used when no token is loaded. Same
    /// shape as a real license so callers don't have to special-case
    /// "no license loaded".
    pub fn free_tier_default(now: DateTime<Utc>) -> Self {
        Self {
            customer_id: "free-tier".to_string(),
            plan_tier: LicenseTier::Free,
            memory_cap: FREE_TIER_MEMORY_CAP,
            features: Vec::new(),
            environment: LicenseEnvironment::Desktop,
            host_binding: None,
            // Free tier never expires; set far in the future so the
            // verify path stays uniform.
            expires_at: (now + chrono::Duration::days(36_500)).timestamp(),
            issued_at: now.timestamp(),
            refresh_at: (now + chrono::Duration::days(36_500)).timestamp(),
            jti: None,
        }
    }

    /// True if this license grants the named feature flag.
    pub fn has_feature(&self, name: &str) -> bool {
        self.features.iter().any(|f| f == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LicenseStatus {
    /// Signed correctly and not past `expires_at`.
    Valid,
    /// Past `expires_at` but within the grace window. Writes still
    /// allowed; engine should attempt refresh.
    GracePeriod { seconds_until_hard_expiry: i64 },
    /// Past expiry + grace window. Writes refused, reads still ok.
    Expired,
    /// Signature failed, claims malformed, or environment binding
    /// mismatch. Treated the same as Expired for enforcement.
    Invalid { reason: String },
}

impl LicenseStatus {
    pub fn allows_writes(&self) -> bool {
        matches!(self, LicenseStatus::Valid | LicenseStatus::GracePeriod { .. })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LicenseError {
    #[error("memory cap exceeded: {current} >= {cap}")]
    CapExceeded { current: u64, cap: u64 },
    #[error("license expired past grace window")]
    Expired,
    #[error("license invalid: {0}")]
    Invalid(String),
}

/// Verify a JWT license token against the embedded public key.
///
/// Returns the parsed claims paired with the enforcement status.
/// Invalid tokens still return claims when JWT decoding succeeded —
/// useful for logging/telemetry. Use `status.allows_writes()` to gate
/// hot-path enforcement.
pub fn verify_license(
    token: &str,
    now: DateTime<Utc>,
) -> Result<(LicenseClaims, LicenseStatus), LicenseError> {
    verify_license_with_key(token, now, LICENSE_PUBLIC_KEY_PEM)
}

/// Same as [`verify_license`] but against a caller-supplied public key.
/// The production path always uses the embedded [`LICENSE_PUBLIC_KEY_PEM`];
/// this seam exists so tests can verify against an isolated keypair
/// without committing any private key that matches the embedded one.
pub fn verify_license_with_key(
    token: &str,
    now: DateTime<Utc>,
    public_key_pem: &[u8],
) -> Result<(LicenseClaims, LicenseStatus), LicenseError> {
    let key = DecodingKey::from_ed_pem(public_key_pem)
        .map_err(|e| LicenseError::Invalid(format!("pubkey load: {e}")))?;

    // Validate signature + structure; we handle expiry + grace manually
    // since jsonwebtoken treats expired as a hard failure.
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.required_spec_claims = std::collections::HashSet::new();
    validation.validate_exp = false;

    let data = jsonwebtoken::decode::<LicenseClaims>(token, &key, &validation)
        .map_err(|e| LicenseError::Invalid(format!("decode: {e}")))?;

    let claims = data.claims;

    // Host binding check. Desktop licenses are minted with a machine
    // fingerprint baked into the token by the SaaS at activation time.
    // If a customer copies the JWT to another machine, the fingerprint
    // won't match and the engine refuses the writes.
    if claims.environment == LicenseEnvironment::Desktop {
        if let Some(bound) = claims.host_binding.clone() {
            let actual = fingerprint::machine_fingerprint();
            if bound != actual {
                return Ok((
                    claims,
                    LicenseStatus::Invalid {
                        reason: format!(
                            "host binding mismatch: token bound to {bound}, this machine is {actual}"
                        ),
                    },
                ));
            }
        }
    }

    let now_ts = now.timestamp();
    let status = if now_ts <= claims.expires_at {
        LicenseStatus::Valid
    } else {
        let elapsed = now_ts - claims.expires_at;
        if elapsed < GRACE_PERIOD_SECS {
            LicenseStatus::GracePeriod {
                seconds_until_hard_expiry: GRACE_PERIOD_SECS - elapsed,
            }
        } else {
            LicenseStatus::Expired
        }
    };

    Ok((claims, status))
}

/// Hot-path enforcement: refuses a Save when the current memory count
/// for the scope is at or above the license cap.
///
/// `current_count` is the engine's count of memory items in scope.
/// Caller is responsible for the query; keeping it explicit lets call
/// sites batch it with other Save-path reads.
pub fn check_write_allowed(
    status: &LicenseStatus,
    cap: u64,
    current_count: u64,
) -> Result<(), LicenseError> {
    if !status.allows_writes() {
        return Err(LicenseError::Expired);
    }
    if cap != u64::MAX && current_count >= cap {
        return Err(LicenseError::CapExceeded {
            current: current_count,
            cap,
        });
    }
    Ok(())
}

/// Resolved runtime license. Immutable for the process lifetime in
/// Phase 1; Phase 2 will add a refresh task that swaps fresh claims in
/// after a `refresh_at` round-trip with the SaaS.
///
/// Construct with [`License::load_from_env`] (production) or [`License::free`]
/// (no token, free-tier defaults).
#[derive(Debug, Clone)]
pub struct License {
    pub claims: LicenseClaims,
    pub status: LicenseStatus,
}

impl License {
    /// Free-tier license with no token. Always valid, capped at
    /// [`FREE_TIER_MEMORY_CAP`].
    pub fn free(now: DateTime<Utc>) -> Self {
        Self {
            claims: LicenseClaims::free_tier_default(now),
            status: LicenseStatus::Valid,
        }
    }

    /// Load from env (`MEMORY_LICENSE_TOKEN` or `MEMORY_LICENSE_PATH`)
    /// or the default file at `~/.memmesh/license.jwt`.
    /// Falls back to [`License::free`] if no credential is present.
    /// `tflk_...` license keys are exchanged at the SaaS for a JWT;
    /// raw JWTs are verified locally. Network failures fall back to
    /// free-tier so the engine stays usable while the SaaS is down.
    pub async fn load_from_env(now: DateTime<Utc>) -> Self {
        store::load_from_env(now).await
    }

    /// Build from a raw token. Convenience for tests + CLI.
    pub fn from_token(token: &str, now: DateTime<Utc>) -> Self {
        Self::from_token_with_key(token, now, LICENSE_PUBLIC_KEY_PEM)
    }

    /// Like [`from_token`] but verifies against a caller-supplied public
    /// key. The production path always uses the embedded
    /// [`LICENSE_PUBLIC_KEY_PEM`]; this seam lets tests verify against an
    /// isolated keypair so no private key matching the embedded one is
    /// ever committed.
    pub fn from_token_with_key(token: &str, now: DateTime<Utc>, public_key_pem: &[u8]) -> Self {
        match verify_license_with_key(token, now, public_key_pem) {
            Ok((claims, status)) => Self { claims, status },
            Err(err) => {
                tracing::warn!(%err, "license token failed verification; falling back to free tier");
                Self::free(now)
            }
        }
    }

    /// Cap as an `Option<i64>` matching the existing `quota::ensure_under_cap`
    /// signature. `None` = unlimited. The free-tier u64 cap converts directly
    /// to i64 because `FREE_TIER_MEMORY_CAP` is well under `i64::MAX`.
    pub fn cap(&self) -> Option<i64> {
        match self.claims.memory_cap {
            u64::MAX => None,
            n if n <= i64::MAX as u64 => Some(n as i64),
            _ => None,
        }
    }

    pub fn allows_writes(&self) -> bool {
        self.status.allows_writes()
    }

    pub fn tier(&self) -> LicenseTier {
        self.claims.plan_tier
    }

    pub fn has_feature(&self, name: &str) -> bool {
        self.claims.has_feature(name)
    }

    /// One-line summary for logs / `doctor`.
    pub fn describe(&self) -> String {
        let cap = if self.claims.memory_cap == u64::MAX {
            "unlimited".to_string()
        } else {
            self.claims.memory_cap.to_string()
        };
        let status = match &self.status {
            LicenseStatus::Valid => "valid".to_string(),
            LicenseStatus::GracePeriod { seconds_until_hard_expiry } => {
                format!("grace ({}d remaining)", seconds_until_hard_expiry / 86_400)
            }
            LicenseStatus::Expired => "expired".to_string(),
            LicenseStatus::Invalid { reason } => format!("invalid ({reason})"),
        };
        format!(
            "tier={} cap={} status={} customer={}",
            self.claims.plan_tier.as_str(),
            cap,
            status,
            self.claims.customer_id,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::pkcs8::spki::EncodePublicKey;
    use ed25519_dalek::pkcs8::EncodePrivateKey;
    use ed25519_dalek::SigningKey;
    use jsonwebtoken::{encode, EncodingKey, Header};

    /// Deterministic, isolated test keypair returned as (private_pem,
    /// public_pem). It is generated at runtime and is unrelated to the
    /// embedded `LICENSE_PUBLIC_KEY_PEM`, so nothing in this test module
    /// can forge a token the production engine would accept. `seed`
    /// lets a test mint a *different* key to exercise the wrong-key path.
    fn test_keypair(seed: u8) -> (String, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let private_pem = sk.to_pkcs8_pem(Default::default()).unwrap().to_string();
        let public_pem = sk
            .verifying_key()
            .to_public_key_pem(Default::default())
            .unwrap();
        (private_pem, public_pem)
    }

    /// PEM of the isolated test public key the positive-path tests verify
    /// against (via [`verify_license_with_key`]).
    fn test_public_key() -> Vec<u8> {
        test_keypair(7).1.into_bytes()
    }

    fn sign_claims(claims: &LicenseClaims) -> String {
        let (private_pem, _) = test_keypair(7);
        let key = EncodingKey::from_ed_pem(private_pem.as_bytes()).unwrap();
        let mut header = Header::new(Algorithm::EdDSA);
        header.typ = Some("JWT".to_string());
        encode(&header, claims, &key).unwrap()
    }

    fn paid_claims(now: DateTime<Utc>, days_until_expiry: i64) -> LicenseClaims {
        LicenseClaims {
            customer_id: "cust_abc".to_string(),
            plan_tier: LicenseTier::Pro,
            memory_cap: 10_000,
            features: vec!["lattice".to_string(), "packs".to_string()],
            environment: LicenseEnvironment::Desktop,
            host_binding: None,
            expires_at: (now + chrono::Duration::days(days_until_expiry)).timestamp(),
            issued_at: now.timestamp(),
            refresh_at: (now + chrono::Duration::days(days_until_expiry - 1)).timestamp(),
            jti: Some("tok_xyz".to_string()),
        }
    }

    #[test]
    fn verifies_a_freshly_signed_license_as_valid() {
        let now = Utc::now();
        let claims = paid_claims(now, 30);
        let token = sign_claims(&claims);

        let (decoded, status) =
            verify_license_with_key(&token, now, &test_public_key()).unwrap();
        assert_eq!(decoded.customer_id, "cust_abc");
        assert_eq!(decoded.plan_tier, LicenseTier::Pro);
        assert_eq!(decoded.memory_cap, 10_000);
        assert_eq!(status, LicenseStatus::Valid);
        assert!(status.allows_writes());
    }

    #[test]
    fn detects_expired_license_within_grace() {
        let now = Utc::now();
        // Issued 35 days ago, expired 2 days ago.
        let claims = paid_claims(now - chrono::Duration::days(35), 33);
        let token = sign_claims(&claims);

        let (_, status) = verify_license_with_key(&token, now, &test_public_key()).unwrap();
        match status {
            LicenseStatus::GracePeriod { seconds_until_hard_expiry } => {
                let days = seconds_until_hard_expiry / 86_400;
                assert!(days >= 4 && days <= 5, "expected ~5 days remaining, got {} days", days);
            }
            other => panic!("expected GracePeriod, got {:?}", other),
        }
    }

    #[test]
    fn rejects_expired_past_grace_window() {
        let now = Utc::now();
        // Expired 30 days ago, well past the 7-day grace.
        let claims = paid_claims(now - chrono::Duration::days(60), 30);
        let token = sign_claims(&claims);

        let (_, status) = verify_license_with_key(&token, now, &test_public_key()).unwrap();
        assert_eq!(status, LicenseStatus::Expired);
        assert!(!status.allows_writes());
    }

    #[test]
    fn rejects_tampered_signature() {
        let now = Utc::now();
        let claims = paid_claims(now, 30);
        let mut token = sign_claims(&claims);
        let last_dot = token.rfind('.').unwrap();
        let byte_pos = last_dot + 5;
        let bytes = unsafe { token.as_bytes_mut() };
        bytes[byte_pos] = if bytes[byte_pos] == b'a' { b'b' } else { b'a' };

        let result = verify_license_with_key(&token, now, &test_public_key());
        assert!(matches!(result, Err(LicenseError::Invalid(_))));
    }

    #[test]
    fn rejects_token_signed_by_wrong_key() {
        // Mint a token with an entirely different keypair, then verify it
        // against the test public key — the signatures must not match.
        let now = Utc::now();
        let claims = paid_claims(now, 30);
        let (other_private, _) = test_keypair(99);
        let key = EncodingKey::from_ed_pem(other_private.as_bytes()).unwrap();
        let token = encode(&Header::new(Algorithm::EdDSA), &claims, &key).unwrap();

        assert!(matches!(
            verify_license_with_key(&token, now, &test_public_key()),
            Err(LicenseError::Invalid(_)),
        ));
    }

    #[test]
    fn check_write_allowed_blocks_at_cap() {
        let status = LicenseStatus::Valid;
        assert!(check_write_allowed(&status, 100, 99).is_ok());
        let err = check_write_allowed(&status, 100, 100).unwrap_err();
        assert!(matches!(err, LicenseError::CapExceeded { .. }));
    }

    #[test]
    fn check_write_allowed_blocks_when_expired() {
        let status = LicenseStatus::Expired;
        let err = check_write_allowed(&status, u64::MAX, 0).unwrap_err();
        assert!(matches!(err, LicenseError::Expired));
    }

    #[test]
    fn check_write_allowed_permits_unlimited_cap() {
        let status = LicenseStatus::Valid;
        assert!(check_write_allowed(&status, u64::MAX, u64::MAX - 1).is_ok());
    }

    #[test]
    fn license_free_defaults_to_free_tier() {
        let now = Utc::now();
        let lic = License::free(now);
        assert_eq!(lic.tier(), LicenseTier::Free);
        assert_eq!(lic.cap(), Some(FREE_TIER_MEMORY_CAP as i64));
        assert!(lic.allows_writes());
    }

    #[test]
    fn license_from_token_uses_paid_claims() {
        let now = Utc::now();
        let claims = paid_claims(now, 30);
        let token = sign_claims(&claims);

        let lic = License::from_token_with_key(&token, now, &test_public_key());
        assert_eq!(lic.tier(), LicenseTier::Pro);
        assert_eq!(lic.cap(), Some(10_000));
        assert!(lic.has_feature("lattice"));
        assert!(!lic.has_feature("nonexistent"));
    }

    #[test]
    fn license_from_garbage_token_falls_back_to_free() {
        let now = Utc::now();
        let lic = License::from_token("not a real jwt", now);
        assert_eq!(lic.tier(), LicenseTier::Free);
        assert_eq!(lic.cap(), Some(FREE_TIER_MEMORY_CAP as i64));
    }

    #[test]
    fn rejects_token_bound_to_a_different_machine() {
        // Token says host_binding = "fp-deadbeef..." but this machine
        // computes its own fingerprint; the engine must mark the
        // status Invalid even though the signature is correct.
        let now = Utc::now();
        let mut claims = paid_claims(now, 30);
        claims.host_binding = Some("fp-deadbeefdeadbeef".to_string());
        let token = sign_claims(&claims);

        let (decoded, status) =
            verify_license_with_key(&token, now, &test_public_key()).unwrap();
        // Claims still parse — useful for telemetry.
        assert_eq!(decoded.plan_tier, LicenseTier::Pro);
        // But enforcement refuses.
        match status {
            LicenseStatus::Invalid { reason } => {
                assert!(
                    reason.contains("host binding mismatch"),
                    "expected host binding error, got: {reason}"
                );
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn accepts_token_bound_to_this_machine() {
        // Sign with the current machine's actual fingerprint → must
        // pass.
        let now = Utc::now();
        let mut claims = paid_claims(now, 30);
        claims.host_binding = Some(crate::fingerprint::machine_fingerprint());
        let token = sign_claims(&claims);

        let (_, status) = verify_license_with_key(&token, now, &test_public_key()).unwrap();
        assert_eq!(status, LicenseStatus::Valid);
    }
}
