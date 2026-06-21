// Mint a dev license JWT signed by your local dev signing key.
//
// FOR LOCAL DEV + TESTING ONLY. The signing key is loaded from the path
// in MEMMESH_DEV_SIGNING_KEY_PEM and is intentionally NOT committed to
// this repository — generate your own dev keypair and embed its public
// half in lib.rs (LICENSE_PUBLIC_KEY_PEM). Production tokens are signed
// by the real signing key inside the SaaS billing backend and are never
// minted locally.
//
// Usage:
//   cargo run --example mint_dev_jwt -- <tier> <cap> <days> <features>
//
// Examples:
//   cargo run --example mint_dev_jwt -- pro 10000 30 lattice,packs
//   cargo run --example mint_dev_jwt -- enterprise unlimited 365
//   cargo run --example mint_dev_jwt           # → pro / 10k / 30d / no features
//
// The token is printed to stdout (no trailing newline) so you can pipe
// it directly:
//   export MEMORY_LICENSE_TOKEN=$(cargo run --quiet --example mint_dev_jwt -- pro 10000 30 lattice)

use chrono::{Duration, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use memory_license::{LicenseClaims, LicenseEnvironment, LicenseTier};

/// Loads the dev license *signing* key (Ed25519 PKCS#8 PEM) from the path
/// in `MEMMESH_DEV_SIGNING_KEY_PEM`. The signing key is never committed;
/// generate your own and embed its public half in
/// `crates/license/src/lib.rs` (LICENSE_PUBLIC_KEY_PEM):
///
///   openssl genpkey -algorithm ed25519 -out dev_signing_key.pem
///   openssl pkey -in dev_signing_key.pem -pubout   # embed this
fn load_dev_signing_key() -> EncodingKey {
    let path = std::env::var("MEMMESH_DEV_SIGNING_KEY_PEM").unwrap_or_else(|_| {
        eprintln!(
            "error: MEMMESH_DEV_SIGNING_KEY_PEM is unset. Point it at an Ed25519\n\
             PKCS#8 PEM private key whose public half is embedded in\n\
             crates/license/src/lib.rs. The private key is never committed:\n\
             \n  openssl genpkey -algorithm ed25519 -out dev_signing_key.pem\n  \
             openssl pkey -in dev_signing_key.pem -pubout"
        );
        std::process::exit(2);
    });
    let pem = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("error: reading {path}: {e}");
        std::process::exit(2);
    });
    EncodingKey::from_ed_pem(&pem).expect("dev signing key parse")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tier_str = args.first().map(String::as_str).unwrap_or("pro");
    let cap: u64 = match args.get(1).map(String::as_str) {
        Some("unlimited") | Some("max") => u64::MAX,
        Some(s) => s.parse().expect("cap must be a number or 'unlimited'"),
        None => 10_000,
    };
    let days: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let features: Vec<String> = args
        .get(3)
        .map(|s| s.split(',').filter(|p| !p.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    let plan_tier = match tier_str {
        "free" => LicenseTier::Free,
        "team" => LicenseTier::Team,
        "enterprise" => LicenseTier::Enterprise,
        _ => LicenseTier::Pro,
    };

    let now = Utc::now();
    let claims = LicenseClaims {
        customer_id: "cust_dev_local".to_string(),
        plan_tier,
        memory_cap: cap,
        features,
        environment: LicenseEnvironment::Desktop,
        host_binding: None,
        expires_at: (now + Duration::days(days)).timestamp(),
        issued_at: now.timestamp(),
        refresh_at: (now + Duration::days((days - 1).max(0))).timestamp(),
        jti: Some(format!("dev_{}", now.timestamp())),
    };
    let key = load_dev_signing_key();
    let mut header = Header::new(Algorithm::EdDSA);
    header.typ = Some("JWT".to_string());
    let token = encode(&header, &claims, &key).expect("sign");
    print!("{token}");
}
