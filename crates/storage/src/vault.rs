// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Encrypted secrets vault — credentials the AI can *reference* but never
//! *read*.
//!
//! Model:
//!   - Values are entered by the human (CLI prompt / console form), sealed
//!     with XChaCha20-Poly1305, and stored in a dedicated SQLite file
//!     (`~/.memmesh/vault.db`) — separate from the memory store, and never
//!     surfaced through search / recall / context injection.
//!   - The master key lives in the OS keychain (macOS Keychain / libsecret /
//!     Windows Credential Manager). If no keychain is available (headless),
//!     it falls back to a `0600` key file so the vault still works.
//!   - The AI never gets a "read value" path. It can list names and
//!     **execute-through-vault**: `run(command)` resolves `{{memmesh:NAME}}`
//!     placeholders to plaintext *inside this process*, runs the command, and
//!     returns output with every secret value scrubbed out — so even a
//!     crafted `echo {{memmesh:x}}` can't leak the value back to the model.

use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    XChaCha20Poly1305, XNonce,
};
use once_cell::sync::Lazy;
use rand::RngCore;
use regex::Regex;
use serde::Serialize;
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

const KEYRING_SERVICE: &str = "memmesh";
const KEYRING_USER: &str = "vault-master-key";

static PLACEHOLDER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\{\{memmesh:([A-Za-z0-9_.\-]+)(?:\|([A-Za-z0-9_.\-]+))?\}\}").unwrap());

/// Non-secret metadata about a stored credential (safe to show the model).
#[derive(Debug, Clone, Serialize)]
pub struct SecretMeta {
    pub name: String,
    pub kind: Option<String>,
    pub description: Option<String>,
    pub created: String,
    pub last_used: Option<String>,
}

/// Result of an execute-through-vault run, already scrubbed of secret values.
#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// Names of the secrets that were resolved into the command.
    pub used: Vec<String>,
}

pub struct Vault {
    pool: SqlitePool,
    cipher: XChaCha20Poly1305,
}

impl Vault {
    /// Default vault path: `~/.memmesh/vault.db`.
    pub fn default_path() -> PathBuf {
        let home = std::env::var_os("HOME").unwrap_or_default();
        PathBuf::from(home).join(".memmesh").join("vault.db")
    }

    /// Open (creating if needed) the vault at `db_path`, loading the master
    /// key from the OS keychain (or the file fallback).
    pub async fn open(db_path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut key = load_or_create_master_key()?;
        let cipher = XChaCha20Poly1305::new((&key).into());
        key.zeroize();

        let url = format!("sqlite://{}?mode=rwc", db_path.display());
        let pool = SqlitePoolOptions::new().max_connections(2).connect(&url).await?;
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS secret (
                name        TEXT PRIMARY KEY,
                kind        TEXT,
                description TEXT,
                scope       TEXT,
                project_id  TEXT,
                nonce       BLOB NOT NULL,
                ciphertext  BLOB NOT NULL,
                created     TEXT NOT NULL,
                updated     TEXT NOT NULL,
                last_used   TEXT
            )"#,
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool, cipher })
    }

    fn encrypt(&self, plaintext: &str) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
        let mut nonce = [0u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let ct = self
            .cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("encrypt failed: {e}"))?;
        Ok((nonce.to_vec(), ct))
    }

    fn decrypt(&self, nonce: &[u8], ct: &[u8]) -> anyhow::Result<String> {
        let pt = self
            .cipher
            .decrypt(XNonce::from_slice(nonce), ct)
            .map_err(|e| anyhow::anyhow!("decrypt failed (wrong key?): {e}"))?;
        Ok(String::from_utf8(pt)?)
    }

    /// Store (or replace) a secret. The plaintext is encrypted immediately and
    /// never written to disk in the clear.
    pub async fn set(
        &self,
        name: &str,
        value: &str,
        kind: Option<&str>,
        description: Option<&str>,
        scope: Option<&str>,
        project_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let (nonce, ct) = self.encrypt(value)?;
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            r#"INSERT INTO secret (name, kind, description, scope, project_id, nonce, ciphertext, created, updated)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
               ON CONFLICT(name) DO UPDATE SET
                 kind=?2, description=?3, scope=?4, project_id=?5,
                 nonce=?6, ciphertext=?7, updated=?8"#,
        )
        .bind(name)
        .bind(kind)
        .bind(description)
        .bind(scope)
        .bind(project_id)
        .bind(nonce)
        .bind(ct)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// List secret metadata — names, kinds, descriptions. Never values.
    pub async fn list(&self) -> anyhow::Result<Vec<SecretMeta>> {
        let rows = sqlx::query(
            "SELECT name, kind, description, created, last_used FROM secret ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| SecretMeta {
                name: r.get("name"),
                kind: r.get("kind"),
                description: r.get("description"),
                created: r.get("created"),
                last_used: r.get("last_used"),
            })
            .collect())
    }

    pub async fn exists(&self, name: &str) -> anyhow::Result<bool> {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM secret WHERE name = ?1")
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        Ok(n > 0)
    }

    pub async fn delete(&self, name: &str) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM secret WHERE name = ?1")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Decrypt a single secret. **Human-facing only** (CLI / loopback console
    /// reveal) — never wire this to a model-facing MCP tool. The AI uses
    /// `run()` instead, which scrubs values from output.
    pub async fn reveal(&self, name: &str) -> anyhow::Result<Option<String>> {
        let row = sqlx::query("SELECT nonce, ciphertext FROM secret WHERE name = ?1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let nonce: Vec<u8> = row.get("nonce");
        let ct: Vec<u8> = row.get("ciphertext");
        let val = self.decrypt(&nonce, &ct)?;
        sqlx::query("UPDATE secret SET last_used = ?2 WHERE name = ?1")
            .bind(name)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await
            .ok();
        Ok(Some(val))
    }

    /// Execute-through-vault: resolve `{{memmesh:NAME}}` placeholders in
    /// `command`, run it via the shell, and return output with every secret
    /// value scrubbed. The plaintext lives only in this process, only for the
    /// duration of the run.
    pub async fn run(&self, command: &str) -> anyhow::Result<RunResult> {
        // Each placeholder is {{memmesh:NAME}} or {{memmesh:NAME|FIELD}}. For a
        // structured (multi-field) secret the value is JSON; FIELD selects one
        // key. Collect (full_match, name, field) preserving the exact matched
        // text so we substitute precisely.
        let matches: Vec<(String, String, Option<String>)> = PLACEHOLDER
            .captures_iter(command)
            .map(|c| {
                (
                    c.get(0).unwrap().as_str().to_string(),
                    c[1].to_string(),
                    c.get(2).map(|m| m.as_str().to_string()),
                )
            })
            .collect();

        let mut resolved = command.to_string();
        let mut secrets: Vec<String> = Vec::new();
        let mut cache: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut used: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (full, name, field) in &matches {
            let raw = match cache.get(name) {
                Some(v) => v.clone(),
                None => match self.reveal(name).await? {
                    Some(v) => {
                        cache.insert(name.clone(), v.clone());
                        used.insert(name.clone());
                        v
                    }
                    None => anyhow::bail!("secret '{name}' is not in the vault"),
                },
            };
            let substitution = match field {
                Some(f) => {
                    let obj: serde_json::Value = serde_json::from_str(&raw).map_err(|_| {
                        anyhow::anyhow!("secret '{name}' is a single value; '{name}|{f}' needs a multi-field secret")
                    })?;
                    obj.get(f)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .ok_or_else(|| anyhow::anyhow!("secret '{name}' has no field '{f}'"))?
                }
                None => raw.clone(),
            };
            resolved = resolved.replace(full, &substitution);
            secrets.push(substitution);
        }
        // Also scrub whole raw values (incl. JSON) from output, not just the
        // fields that were substituted.
        for v in cache.values() {
            secrets.push(v.clone());
        }

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&resolved)
            .output()
            .await;
        // Wipe the resolved command (contains plaintext) regardless of outcome.
        resolved.zeroize();
        let output = output?;

        let scrub = |mut s: String| -> String {
            for sec in &secrets {
                if !sec.is_empty() {
                    s = s.replace(sec.as_str(), "[redacted]");
                }
            }
            s
        };
        let result = RunResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: scrub(String::from_utf8_lossy(&output.stdout).into_owned()),
            stderr: scrub(String::from_utf8_lossy(&output.stderr).into_owned()),
            used: used.into_iter().collect(),
        };
        secrets.iter_mut().for_each(|s| s.zeroize());
        Ok(result)
    }
}

// ── master key management ───────────────────────────────────

fn key_file_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".memmesh").join("vault.key")
}

/// Load the 32-byte master key from the OS keychain, or create one. Falls back
/// to a `0600` key file when no keychain backend is usable (headless hosts) so
/// the vault degrades to file-based protection rather than failing outright.
fn load_or_create_master_key() -> anyhow::Result<[u8; 32]> {
    match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        Ok(entry) => match entry.get_password() {
            Ok(hex) => return hex_to_key(&hex),
            Err(keyring::Error::NoEntry) => {
                let key = random_key();
                if entry.set_password(&key_to_hex(&key)).is_ok() {
                    return Ok(key);
                }
                // keychain write failed → fall through to file
            }
            Err(_) => { /* keychain unusable → fall through to file */ }
        },
        Err(_) => { /* no keychain backend → fall through to file */ }
    }
    load_or_create_key_file()
}

fn load_or_create_key_file() -> anyhow::Result<[u8; 32]> {
    let path = key_file_path();
    if path.exists() {
        let hex = std::fs::read_to_string(&path)?;
        return hex_to_key(hex.trim());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let key = random_key();
    std::fs::write(&path, key_to_hex(&key))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    tracing::warn!(path = %path.display(), "OS keychain unavailable — vault master key stored in a 0600 file");
    Ok(key)
}

fn random_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    OsRng.fill_bytes(&mut k);
    k
}

fn key_to_hex(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_to_key(hex: &str) -> anyhow::Result<[u8; 32]> {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("corrupt master key: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("master key is not 32 bytes"))?;
    Ok(arr)
}
