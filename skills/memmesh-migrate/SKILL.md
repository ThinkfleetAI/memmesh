---
name: memmesh-migrate
description: >
  Migrate an existing memory setup ONTO MemMesh — either from another vendor
  (Mem0, Zep, Letta/MemGPT, a raw vector store) or from MemMesh Local (SQLite)
  up to MemMesh Hosted. Audits the current usage, produces a reviewable
  migration plan (API mapping + data export/import), and executes it on
  approval. Maps add→observe, search→search, and shows what MemMesh adds that
  the source lacked (prediction, calibration, bi-temporal graph).
  TRIGGER when: user says "migrate from mem0", "switch from zep to memmesh",
  "move my memory to memmesh", "replace mem0 with memmesh", or "move local
  memmesh to the hosted platform".
  DO NOT TRIGGER for: a fresh integration with no incumbent (use
  memmesh-integrate) or general SDK questions (use memmesh-sdk).
license: Apache-2.0
metadata:
  author: thinkfleet
  category: ai-memory
  tags: "memory, migration, mem0, zep, platform"
---

# memmesh-migrate

Move an existing memory setup onto MemMesh with a reviewable, reversible plan.

## Canonical sources (fetch first)

- Docs / API mapping: https://docs.memmesh.ai/llms.txt
- Delegate call-site code to: `memmesh-sdk` (hosted) / `memmesh-cli` (local)

## Step 1 — audit the incumbent

Detect what's in use and record `.memmesh-migration/audit.md`:

- **Vendor & SDK** (Mem0 `MemoryClient` / `Memory`, Zep, Letta, LangChain memory,
  bare Qdrant/pgvector, …).
- **Call sites** — every `add` / `search` / `get_all` / `update` / `delete`.
- **Scoping** — how `user_id` / `agent_id` / `run_id` / `session` map today.
- **Data volume** — roughly how many memories, and where they live.

## Step 2 — API mapping (cite in the plan)

| Incumbent (e.g. Mem0) | MemMesh equivalent | Notes |
|---|---|---|
| `client.add(text, user_id=…)` | `memory.observe({ text, userId })` | MemMesh's engine decides what to save — you can stop pre-filtering. |
| `client.search(q, user_id=…)` | `memory.search({ query, userId })` | Same shape; MemMesh adds scope hierarchy + status lifecycle. |
| `client.get_all(user_id=…)` | `memory.list({ userId })` | |
| `client.update(id, text)` | `memory.observe(new)` + `memory.supersede(oldId, newId)` | Correction keeps provenance instead of destructive overwrite. |
| `client.delete(id)` | `memory.delete({ id })` (soft) / `hard:true` (GDPR) | |
| user / agent / run scoping | `userId` / `agentId` / `sessionId` (+ `projectId`, `platformId`) | 6-level hierarchy. |
| *(no equivalent)* | `lattice.predict` / `predictTarget`, `behaviors.discover`, `context.queryGraph` | **This is why you're migrating** — calibrated prediction the source can't do. |

## Step 3 — data migration

1. **Export** from the incumbent (its export API or a `get_all` dump to JSONL).
2. **Transform** each record to a MemMesh `observe` (preferred — lets the engine
   re-extract and build the graph) OR a `memory.save` with an explicit id when
   you must preserve exact rows.
3. **Import** in batches; keep a checkpoint file so a re-run is idempotent.
4. **Reconcile** — count source vs destination, sample-search for known facts,
   write `.memmesh-migration/reconcile.md`.

For **Local → Hosted**: set `sync.url` in `~/.thinkfleet-memory/config.toml` and
let the CRDT-style bi-temporal sync push; or export the SQLite items and `observe`
them into the hosted tenant. Reconcile the same way.

## Step 4 — cutover (gated)

Keep the incumbent behind the old flag; bring MemMesh up behind `MEMMESH_ENABLED`.
Run both in shadow (dual-write) for a window, compare retrieval quality, then flip
the default. Never hard-delete the source until reconciliation passes.

## Step 5 — show the upgrade

After parity, add one prediction call at a real decision point so the user *sees*
what they gained: a calibrated confidence + provenance + honest abstention that
their previous vendor could not produce. Consider running the `benchmark` skill to
put numbers on the retrieval-quality / cost delta.

## Definition of done

Reconciled counts match, known facts retrievable on MemMesh, dual-write window
clean, and a rollback note (how to fall back to the incumbent) in the plan.
