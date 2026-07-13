---
name: peek
description: >
  Quick memory lookup — search MemMesh and show compact one-liner results, or
  fetch one memory by id. Use for fast checks ("did we record X?"), resolving a
  [memmesh:id] citation, or browsing without full detail.
---

# peek

Fast, low-noise lookup.

## By query
```jsonc
{ "name": "memory_search", "arguments": { "query": "<term>", "projectId": "<repo>", "limit": 8 } }
```
Render each hit as a single line: `[id-prefix] content — scope/type`. Don't dump
full JSON.

## By id
```jsonc
{ "name": "memory_recall", "arguments": { "id": "<id>" } }
```
`memory_recall` also bumps recency (marks the item recently used). Use it to
resolve a `[memmesh:<id>]` citation to its full content.

## When to escalate

If the user wants everything grouped by category, use `tour`. If they want a
synthesized picture of one subject (profile + patterns + predictions), use
`context-loader` / `memory_build_context`.
