// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.
// Proprietary and confidential.

//! Pure domain algorithms for the memory engine.
//!
//! No I/O. No async runtime. No storage. Everything here is:
//!   - Pattern detectors (recurring-event, day-of-week, time-of-day, etc.)
//!   - Hybrid scorer (keyword + vector + recency + confidence)
//!   - Bi-temporal helpers (validFrom / validTo / learnedAt / lastAccessedAt)
//!   - Token-budgeted assembly
//!
//! Anything that touches a database, an embedding service, or a network call
//! belongs in another crate. The strict separation makes testing trivial and
//! keeps the storage backends (`SqliteStore`, `PostgresStore`) free to share
//! the same algorithm code.

pub mod audit;
pub mod bindings;
pub mod block;
pub mod config;
pub mod contact;
pub mod extraction;
pub mod feedback;
pub mod graph;
pub mod model;
pub mod patterns;
pub mod scoring;
pub mod typed;
pub mod user_profile;

pub use audit::MemoryAuditEvent;
pub use bindings::{sync_state_key, sync_status, ProjectBinding};
pub use block::MemoryBlock;
pub use contact::{Contact, ContactEvent};
pub use feedback::MemoryFeedback;
pub use graph::{MemoryEdge, MemoryEntity};
pub use model::*;
pub use typed::{
    Accumulator, AttributeDef, DataType, IngestReport, ObservationStatus, TypedObservation,
};
pub use user_profile::UserProfile;
