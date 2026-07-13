---
name: export
description: >
  Export MemMesh memories for a project/user to a portable Markdown (or JSONL)
  file for backup, migration, or sharing. Use when backing up, moving to another
  project, sharing memory state with teammates, or archiving before a cleanup.
---

# export

Dump memory to a portable file.

## Pull the set
```jsonc
{ "name": "memory_list", "arguments": { "projectId": "<repo>", "limit": 1000 } }
```
Page with `offset` if the store is large (check `memory_stats` for the total).

## Write the file

Default to Markdown grouped by `type`, one memory per bullet with its id, so it
round-trips through the `import` skill:

```markdown
# MemMesh export — <project> — <count> items
## preference
- [<id>] prefers pnpm over npm  <!-- scope:user importance:6 -->
## rule
- [<id>] all API routes require auth middleware  <!-- scope:project impact:HIGH -->
```

For a machine-readable backup, write JSONL (one memory object per line) instead —
preserves ids, timestamps, and status for exact restore.

## Where

Ask for a path, or default to `./memmesh-export-<project>-<date>.md` in the repo.
For a full compliance-grade export of one subject (audit trail included), use the
SDK's `compliance.exportSubject` instead.

## Next

To move onto another project or a teammate's machine, hand the file to `import`.
To switch off another vendor entirely, use `memmesh-migrate`.
