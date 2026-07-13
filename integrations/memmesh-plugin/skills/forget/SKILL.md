---
name: forget
description: >
  Delete or correct a MemMesh memory. Finds the item by search or id, confirms,
  then soft-deletes (default, sync-safe) or hard-deletes (GDPR/cleanup). Also
  handles "undo that" for a memory just added, and corrections via supersede.
  Use when removing outdated/incorrect/sensitive memories or cleaning up after
  experiments.
---

# forget

Remove or correct what's in memory. **Always confirm before deleting.**

## 1. Find it

```jsonc
{ "name": "memory_search", "arguments": { "query": "<what to forget>", "projectId": "<repo>", "limit": 10 } }
```

Show the matches (id + content) and ask which to remove.

## 2. Delete

```jsonc
{ "name": "memory_delete", "arguments": { "id": "<id>" } }          // soft: status→rejected, sync-safe
{ "name": "memory_delete", "arguments": { "id": "<id>", "hard": true } }  // physical: GDPR / cleanup only
```

Default to **soft** delete — it stops surfacing in search and propagates the
rejection to peer stores over sync. Use `hard: true` only for right-to-forget or
operator cleanup.

## Correction vs deletion

If the fact **changed** (not "was wrong to store"), don't delete — record a
correction so provenance survives: `memory_observe` the new statement, then
`memory_supersede` the stale id `byId` the new one. The engine also supersedes
automatically when you observe a contradiction, so plain `memory_observe` is
often enough.

## Undo a just-added memory

If the user says "undo that" right after a save, search for the most recent item
in scope (`memory_list`), confirm it's the one, and `memory_delete` it.
