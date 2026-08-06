// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Reproducible evaluation of the memory engine's retrieval quality.
//!
//! Each fixture is a YAML file describing a synthetic memory corpus plus a
//! list of queries, each with an expected set of relevant memory IDs. The
//! runner seeds a fresh in-memory SQLite store, executes each query
//! through the engine's normal retrieval path, and scores the returned
//! ranking against the expected set.
//!
//! Three scores per query, mirroring the metrics gbrain's BrainBench
//! reports (so cross-comparison is straightforward later):
//!
//!   P@K  precision at K — fraction of returned top-K that were expected
//!   R@K  recall at K    — fraction of expected items that appear in top K
//!   MRR  mean reciprocal rank of the FIRST relevant result
//!
//! A fixture's scorecard rolls these into per-query rows plus an aggregate
//! line. Multiple fixtures roll into a suite scorecard so CI / `eval`
//! invocations can diff against a baseline.
//!
//! NOT a benchmark of throughput, latency, or memory footprint. This is
//! purely a retrieval-quality regression net so changes to extraction,
//! ranking, or scoring are provably-helpful before they ship.

pub mod fixture;
pub mod runner;
pub mod scorer;

pub use fixture::{Fixture, FixtureCorpusItem, FixtureQuery};
pub use runner::{run_fixture, run_suite, SuiteResult};
pub use scorer::{QueryScore, Scorecard};
