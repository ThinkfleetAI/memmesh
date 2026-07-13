---
name: switch-project
description: >
  Override the auto-detected project scope for MemMesh reads/writes, or widen to
  cross-project / user-level search. Use when working across repos, pulling a
  decision from another project, or when auto-detection resolved to the wrong
  projectId.
---

# switch-project

Change which scope memory operations target.

## Default detection

By default `projectId` = the git repo directory name, `userId` = the OS user,
`platformId` = `local`. That's usually right.

## Point at another project

Pass an explicit `projectId` on the call:
```jsonc
{ "name": "memory_search", "arguments": { "projectId": "other-repo", "limit": 20 } }
```
For the rest of the task, keep using that `projectId` on `observe` / `save` /
`search` so reads and writes stay consistent.

## Widen the search

- **User-level** (personal preferences, cross-project): drop `projectId`, pass
  `userId` and `scope: "user"`.
- **Everything under the platform**: pass only `platformId` (and optionally
  `scope`). Use sparingly — it can be noisy.

## Confirm the switch

Tell the user which scope you're now reading/writing, and switch back when the
cross-project detour is done, so you don't accidentally write memories into the
wrong project.
