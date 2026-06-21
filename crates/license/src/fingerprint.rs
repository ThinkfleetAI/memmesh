// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Machine fingerprint used for desktop license host binding.
//!
//! The fingerprint is sent to the activation server when redeeming a
//! purchase code; the server bakes it into the JWT's `host_binding`
//! claim. At verify time the engine recomputes the local fingerprint
//! and refuses tokens whose binding doesn't match — stops casual
//! sharing of a single license across machines.
//!
//! Implementation: hash of `(hostname, OS username)`. Stable for the
//! lifetime of an installed OS user account, doesn't move when the
//! customer moves their `~/.memmesh/` directory between
//! disks, and doesn't require admin-level OS APIs.
//!
//! Falls back to "unknown-host" / "unknown-user" when either component
//! can't be read; this still produces a stable per-machine fingerprint
//! (collisions only happen between machines that both fail the same
//! way, which is acceptable for the threat model).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Stable per-machine fingerprint. Same machine + same OS user →
/// same value, every run, no network calls.
pub fn machine_fingerprint() -> String {
    let host = hostname().unwrap_or_else(|| "unknown-host".to_string());
    let user = os_user();
    let mut h = DefaultHasher::new();
    host.hash(&mut h);
    user.hash(&mut h);
    format!("fp-{:016x}", h.finish())
}

/// Human-readable machine label shown to the customer in the
/// activation dashboard. Just the hostname, no hashing.
pub fn machine_label() -> String {
    hostname().unwrap_or_else(|| "unknown-host".to_string())
}

fn os_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown-user".to_string())
}

fn hostname() -> Option<String> {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    if let Ok(h) = std::env::var("COMPUTERNAME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    // macOS / Linux: read /etc/hostname or shell out to `hostname`.
    // /etc/hostname is the cheap path; shelling out adds an extra
    // process spawn we don't need on the hot path.
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        let h = s.trim();
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }
    // Last resort on macOS where /etc/hostname is empty by default:
    // shell out to `hostname`. Activate is rare (once per install) so
    // the process spawn cost doesn't matter.
    if let Ok(out) = std::process::Command::new("hostname").output() {
        if out.status.success() {
            let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !h.is_empty() {
                return Some(h);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_within_run() {
        let a = machine_fingerprint();
        let b = machine_fingerprint();
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_has_expected_shape() {
        let fp = machine_fingerprint();
        assert!(fp.starts_with("fp-"), "expected fp- prefix, got {fp}");
        assert_eq!(fp.len(), 19, "expected fp-NNNNNNNNNNNNNNNN (19 chars), got {fp}");
    }

    #[test]
    fn label_is_non_empty() {
        let label = machine_label();
        assert!(!label.is_empty());
    }
}
