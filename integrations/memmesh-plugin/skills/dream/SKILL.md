---
name: dream
description: >
  Consolidate MemMesh memories — find duplicates and contradictions, merge or
  supersede them, and retire stale entries — to keep search results clean. Use
  when memory count is high, search feels noisy/repetitive, or for periodic
  hygiene. Respects pinned memories.
---

# dream

Agent-driven consolidation. MemMesh keeps provenance, so consolidation is
*supersede/reject*, not destructive rewrite.

## 1. Survey
```jsonc
{ "name": "memory_stats", "arguments": { "projectId": "<repo>" } }
```
A high `total` or a large `superseded`/`rejected` share signals it's worth a pass.

## 2. Find redundancy

Pull the set (`memory_list`) or search hot topics, and identify:
- **Duplicates** — same fact stored multiple times.
- **Contradictions** — two memories that can't both be true.
- **Stale** — superseded facts still cluttering results, or one-off noise.

## 3. Consolidate (confirm first; never touch pinned items)

- **Contradiction / changed fact** → keep the newest, `memory_supersede` the
  older `byId` the newer. Provenance is preserved.
- **Exact duplicate** → keep one, `memory_delete` the rest (soft).
- **Stale noise** → `memory_delete` (soft) after confirming with the user.

Do **not** delete anything marked pinned (high importance / impact HIGH /
confirmed) — see the `pin` skill. When in doubt, supersede rather than delete.

## 4. Report

Summarize: N duplicates merged, M contradictions resolved, K stale retired, and
the new total. Suggest re-running when stats drift again.

> Hosted tenants can offload this to the server-side consolidator
> (`memory.consolidate` / `dedup` in the SDK); locally, this agent-driven pass is
> the consolidation path.
