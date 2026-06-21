# memmesh — Architecture

> Part of [MemMesh](https://memmesh.ai). Licensed under Apache-2.0.

## Goals

1. **Same binary, two runtimes.** Postgres for SaaS / multi-tenant; SQLite for
   desktop / single-tenant. Mode chosen at startup, never at compile time.
2. **Feature parity, day one.** Local mode is not a degraded experience. Full
   hierarchical memory, full lattice patterns, full hybrid retrieval, full
   audit, full graph extraction.
3. **Schema parity with the existing TypeScript API** during transition. Rust
   engine reads/writes the same Postgres tables the activepieces TS API uses
   (`agent_memory_item`, `memory_audit_event`, `clawdbot_memory_item`,
   `lattice_*`). SQLite mirrors that schema row-for-row so sync is data-level,
   not translation-level.
4. **License-gated.** Server mode requires a signed license key; local SQLite
   mode is free-tier.

## Crate layout

```
crates/
  core/        Pure algorithms. No I/O. Pattern detectors, scoring, bi-temporal helpers.
  storage/     `Storage` trait + SqliteStore + PostgresStore. The only place SQL lives.
  server/      gRPC + MCP entrypoint binary. Drives license gate, audit, sync wiring.
  mcp/         Universal MCP protocol layer.
  sync/        Bi-temporal CRDT reconciliation between local + remote Storage.
  audit/       Access log + DLP pre-save hooks. Streams to Shield when configured.
  license/     Ed25519 signed license-key verification.
  cli/         `memmesh` CLI.
```

## Storage trait — the cornerstone

Every downstream crate talks to `memory_storage::Storage`. No crate outside
`storage/` knows whether the backend is SQLite or Postgres. This makes:

- Tests trivial (SqliteStore against an in-memory file).
- Local + SaaS swap a runtime concern.
- Sync engine clean — sync is `Storage` ↔ `Storage`.

## Mode selection (runtime, never compile-time)

```sh
# Local (desktop)
memmesh-server --storage=sqlite:./memory.db

# SaaS (server)
memmesh-server --storage=postgres --license-key-file=/etc/thinkfleet/license.key

# Hybrid (desktop + sync to remote)
memmesh-server --storage=sqlite:./memory.db --sync=https://api.memmesh.ai
```

Cargo features (`sqlite`, `postgres`) gate optional capabilities so the binary
can be built minimally if desired. Default features include both.

## Phase plan

**Phase 1 — Full-parity local engine + sync to SaaS.** All 15 todo items in the
working list. Schema parity with the TS API. SQLite + Postgres backends both
work. MCP server functional. Sync engine working bidirectional.

**Phase 2 — Polish + ecosystem.** TS / Python SDKs, dashboard UI in
ThinkFleet Desktop, license server infra (heartbeat + revocation), advanced
DLP rules.

**Phase 3 — SaaS swap-in.** activepieces TS API switches to use this engine
as its memory backend. Legacy lattice-engine crate in
`packages/server/lattice-engine/` retires. Rust + sqlx become the canonical
schema owner.

## Non-goals (right now)

- Public source release under Apache-2.0.
- Multi-region / sharding (Postgres handles it for SaaS for now).
- Embedded vector model — embeddings come from a remote provider (OpenAI,
  Anthropic, Voyage) via a pluggable adapter. No model weights in the binary.
- Mobile build. Desktop is the distribution vehicle.
