// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.
//
// Typed-observation ingest benchmark — validates that the partition-ready
// schema sustains high write throughput and index-backed range queries, the
// "design + benchmark" evidence for the billions-of-records target.
//
// Requires a Postgres with the engine schema. Usage:
//
//   THINKFLEET_TEST_PG_URL=postgres://user:pass@host/db \
//     cargo run -p memory-storage --example bench_typed_ingest --release -- 1000000 5000
//
// args: [total_rows] [batch_size]   (defaults: 200_000, 2_000)
//
// It spreads rows across many synthetic subjects (so accumulators and the hash
// partitions both see realistic fan-out), reports ingest throughput, then times
// a numeric range query and an accumulator read. Cleans up after itself.

use std::time::Instant;

use memory_core::{AttributeDef, DataType, TypedObservation};
use memory_storage::{
    postgres::PostgresStore,
    query::{AccumulatorKey, TypedObservationFilter},
    Storage,
};

const PLATFORM: &str = "bench_typed_platform";
const ATTR: &str = "credit_score";

#[tokio::main]
async fn main() {
    let Ok(url) = std::env::var("THINKFLEET_TEST_PG_URL") else {
        eprintln!("set THINKFLEET_TEST_PG_URL to run this benchmark");
        return;
    };
    let args: Vec<String> = std::env::args().collect();
    let total: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let batch_size: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2_000);
    let subjects: usize = (total / 1000).max(1); // ~1000 observations per subject

    let store = PostgresStore::connect(&url).await.expect("connect");
    store.migrate().await.expect("migrate");
    cleanup(&store).await;

    store
        .upsert_attribute_def(&AttributeDef {
            id: "bench_def".into(),
            platform_id: PLATFORM.into(),
            project_id: None,
            attribute_key: ATTR.into(),
            data_type: DataType::Numeric,
            unit: None,
            min_valid: Some(300.0),
            max_valid: Some(850.0),
            required: false,
            metadata: serde_json::Value::Null,
        })
        .await
        .expect("register def");

    println!(
        "ingesting {total} observations across {subjects} subjects in batches of {batch_size}..."
    );
    let start = Instant::now();
    let mut done = 0usize;
    let mut seq = 0u64;
    while done < total {
        let n = batch_size.min(total - done);
        let mut batch = Vec::with_capacity(n);
        for _ in 0..n {
            // Deterministic spread without Math.random (unavailable): hash the
            // sequence number into a subject + a plausible 300..850 value.
            let subject = seq % subjects as u64;
            let value = 300.0 + (seq % 551) as f64; // 300..=850
            let observed = chrono::Utc::now();
            let mut o = TypedObservation::numeric(
                format!("bo_{seq}"),
                PLATFORM,
                "contact",
                format!("subj_{subject}"),
                ATTR,
                value,
                observed,
            );
            o.project_id = Some("bench_proj".into());
            batch.push(o);
            seq += 1;
        }
        store.save_typed_observations(&batch).await.expect("ingest");
        done += n;
    }
    let elapsed = start.elapsed();
    let rate = total as f64 / elapsed.as_secs_f64();
    println!(
        "ingest: {total} rows in {:.2}s = {:.0} rows/sec",
        elapsed.as_secs_f64(),
        rate
    );

    // Index-backed cross-subject range query.
    let q0 = Instant::now();
    let rows = store
        .query_typed_observations(&TypedObservationFilter {
            platform_id: Some(PLATFORM.into()),
            attribute_key: Some(ATTR.into()),
            min_value: Some(600.0),
            max_value: Some(620.0),
            limit: Some(1000),
            ..Default::default()
        })
        .await
        .expect("range query");
    println!(
        "range query [600,620] returned {} rows in {:.2}ms",
        rows.len(),
        q0.elapsed().as_secs_f64() * 1000.0
    );

    // Accumulator read (O(1) running stats).
    let a0 = Instant::now();
    let acc = store
        .get_accumulator(&AccumulatorKey {
            platform_id: PLATFORM.into(),
            subject_kind: "contact".into(),
            subject_external_id: "subj_0".into(),
            attribute_key: ATTR.into(),
        })
        .await
        .expect("accumulator");
    if let Some(acc) = acc {
        println!(
            "accumulator subj_0: count={} mean={:.1} in {:.2}ms",
            acc.count,
            acc.mean().unwrap_or(0.0),
            a0.elapsed().as_secs_f64() * 1000.0
        );
    }

    cleanup(&store).await;
    println!("done (cleaned up).");
}

async fn cleanup(store: &PostgresStore) {
    for sql in [
        "DELETE FROM memory_ingest_ledger WHERE \"observationId\" LIKE 'bo_%'",
        "DELETE FROM memory_typed_observation WHERE \"platformId\" = $1",
        "DELETE FROM memory_attribute_accumulator WHERE \"platformId\" = $1",
        "DELETE FROM memory_attribute_registry WHERE \"platformId\" = $1",
    ] {
        let q = if sql.contains("$1") {
            sqlx::query(sql).bind(PLATFORM)
        } else {
            sqlx::query(sql)
        };
        q.execute(store.pool()).await.ok();
    }
}
