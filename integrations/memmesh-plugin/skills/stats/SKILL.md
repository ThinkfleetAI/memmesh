---
name: stats
description: >
  Show MemMesh usage stats for a project/user — total count and breakdowns by
  type, scope, and status, plus the oldest/newest timestamps. Use when checking
  "how many memories do I have", auditing distribution before a cleanup, or
  giving a quick health read.
---

# stats

Summarize what's in memory.

```jsonc
{ "name": "memory_stats", "arguments": { "projectId": "<repo>" } }
```

Scope it with `projectId` / `userId` / `scope`; omit for everything under the
platform. Returns `total`, `byType`, `byScope`, `byStatus`, `oldest`, `newest`,
and `scanCapped` (true if the count hit the scan limit — raise `limit` for an
exact number on very large stores).

## Present it

Lead with the total, then the type breakdown (the useful one), then flag health
signals:
- a large `superseded` / `rejected` share ⇒ suggest `dream` (consolidation).
- approaching the free-tier 500-item cap ⇒ mention it and suggest `forget`/`dream`.

For a per-subject picture (not aggregate counts) use `context-loader`.
