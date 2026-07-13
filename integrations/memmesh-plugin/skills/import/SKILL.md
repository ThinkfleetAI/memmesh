---
name: import
description: >
  Import memories into MemMesh from an exported file, a native MEMORY.md /
  CLAUDE.md, an ADR/decision log, or a mem0 export. Use when migrating from
  another project, restoring a backup, or seeding a new project with existing
  knowledge.
---

# import

Load durable knowledge into MemMesh.

## 1. Read the source

Accept: a MemMesh export (Markdown or JSONL), a native `MEMORY.md` /
`CLAUDE.md`, an ADR folder, or a **mem0 export** dump.

## 2. Feed it in — prefer observe

For prose / notes / decisions, `memory_observe` each item so the engine
re-extracts and builds the knowledge graph:

```jsonc
{ "name": "memory_observe", "arguments": { "text": "<one fact/decision>", "projectId": "<repo>" } }
```

For a MemMesh JSONL backup where you must preserve exact ids/timestamps, use
`memory_save` per row instead.

## 3. Scope it

Project rules/decisions → `scope: "project"` with `projectId`. Personal
preferences → `scope: "user"` with `userId`. Batch by scope so it's consistent.

## 4. Idempotency & verify

`memory_observe` dedupes re-observed text, so a re-run is safe. After import,
run `stats` to confirm the count, and `peek` a couple of known facts to confirm
they're retrievable.

> Importing a full mem0/Zep setup (not just a file)? Use `memmesh-migrate` — it
> audits call sites, maps the API, and reconciles counts.
