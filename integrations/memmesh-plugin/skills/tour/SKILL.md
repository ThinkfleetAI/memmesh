---
name: tour
description: >
  Browse all stored MemMesh memories for a project/user, grouped by type/scope
  with full content. Use when reviewing everything captured, onboarding to a
  project, or getting an overview of decisions, conventions, and learnings.
---

# tour

Show the full contents of memory, organized.

## Pull the set
```jsonc
{ "name": "memory_list", "arguments": { "projectId": "<repo>", "limit": 100 } }
```
For a personal overview, pass `userId` and `scope: "user"` instead.

## Present it

Group by `type` (preference / fact / rule / decision / behavior_pattern / …),
then within each show `content` with a short id prefix. Call out anything
`status: superseded` separately so the user sees what's been replaced.

End with a one-line summary: total count and the type breakdown (that's exactly
what the `stats` skill returns if you want the numbers).

## Big memory sets

If `memory_list` hits the limit, page with `offset`, or narrow by `scope` /
`type`. Suggest `dream` (consolidation) if the tour reveals lots of duplicates
or contradictions.
