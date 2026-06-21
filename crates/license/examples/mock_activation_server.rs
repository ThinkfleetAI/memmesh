// Tiny mock of api.memmesh.ai's activation endpoint.
//
// FOR LOCAL DEV + INTEGRATION TESTING ONLY. Simulates the SaaS response
// shape so the engine's `activate` flow can be exercised end-to-end
// without a real billing backend. Signs the returned JWT with your local
// dev signing key, loaded from the path in MEMMESH_DEV_SIGNING_KEY_PEM —
// generate your own dev keypair and embed its public half in the engine's
// lib.rs. The signing key is never committed to this repository.
//
// Usage:
//   cargo run --example mock_activation_server -p memory-license
//   # Listens on http://127.0.0.1:8765 by default.
//   # Override port: MOCK_ACTIVATION_PORT=9999 cargo run --example ...
//
// Then in another terminal:
//   memmesh activate TF-MOCK-CODE \
//     --endpoint http://127.0.0.1:8765/api/v1/license/activate

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

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

#[derive(serde::Deserialize)]
struct ActivateRequest {
    code: String,
    machine_fingerprint: String,
    machine_label: String,
}

fn main() {
    let port: u16 = std::env::var("MOCK_ACTIVATION_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8765);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .unwrap_or_else(|e| panic!("bind 127.0.0.1:{port}: {e}"));
    eprintln!(
        "mock activation server listening on http://127.0.0.1:{port}/api/v1/license/activate"
    );
    for stream in listener.incoming() {
        match stream {
            Ok(s) => handle(s),
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
}

fn handle(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    // Read headers — accumulate until blank line.
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }

    // Read body.
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }

    // Tolerate non-JSON bodies in the simplest way: 400 back.
    let parsed: Result<ActivateRequest, _> = serde_json::from_slice(&body);
    let resp = match parsed {
        Ok(req) => {
            eprintln!(
                "  ◀ activate code={} fingerprint={} label={}",
                req.code, req.machine_fingerprint, req.machine_label,
            );
            // Mint a Pro JWT bound to this caller's fingerprint. The
            // host_binding ensures the engine refuses the token if it
            // gets moved to another machine.
            let now = Utc::now();
            let days = 365i64;
            let claims = LicenseClaims {
                customer_id: "cust_mock".to_string(),
                plan_tier: LicenseTier::Pro,
                memory_cap: 100_000,
                features: vec!["lattice".to_string(), "packs".to_string()],
                environment: LicenseEnvironment::Desktop,
                host_binding: Some(req.machine_fingerprint),
                expires_at: (now + Duration::days(days)).timestamp(),
                issued_at: now.timestamp(),
                refresh_at: (now + Duration::days(days - 1)).timestamp(),
                jti: Some(format!("mock_{}", now.timestamp())),
            };
            let key = load_dev_signing_key();
            let mut header = Header::new(Algorithm::EdDSA);
            header.typ = Some("JWT".to_string());
            let token = encode(&header, &claims, &key).unwrap();
            let body = serde_json::json!({
                "token": token,
                "tier": "pro",
                "memory_cap": 100_000u64,
                "features": ["lattice", "packs"],
                "expires_at": chrono::DateTime::<Utc>::from_timestamp(claims.expires_at, 0)
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_default(),
            });
            let body_str = body.to_string();
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body_str}",
                body_str.len(),
            )
        }
        Err(e) => {
            let body = serde_json::json!({ "error": format!("bad request: {e}") }).to_string();
            format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len(),
            )
        }
    };
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}
