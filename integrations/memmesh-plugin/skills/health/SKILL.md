---
name: health
description: >
  Diagnose MemMesh connectivity and correctness — is the MCP server reachable,
  is the key/config valid, do read and write actually work? Use when memory
  operations fail, searches return empty unexpectedly, observe/save errors
  occur, or to confirm the plugin is wired correctly.
---

# health

Diagnose the MemMesh plugin when something seems off.

## 1. Wiring
```bash
memmesh doctor        # binary on PATH, MCP config written, skill present, hooks
```
If the MCP server isn't listed by the agent, the config didn't take — re-run
`npx @thinkfleet/memmesh install --force` and restart the tool.

## 2. Read/write round-trip

Prove the engine actually works, end to end:

1. `memory_save` a throwaway item (`type: "fact"`, `scope: "session"`, content
   `"memmesh healthcheck <runid>"`).
2. `memory_search` for `"healthcheck <runid>"` — assert it comes back.
3. `memory_delete` (hard) the throwaway so nothing lingers.

If step 1 fails with a cap error, you're at the free-tier 500-item limit — see
`stats` and suggest `forget`/`dream`.

## 3. Common failures

| Symptom | Likely cause | Fix |
|---|---|---|
| No MemMesh tools visible | MCP config missing | `memmesh install --force`, restart tool |
| `rejected: ... cap` | free-tier 500 cap hit | `dream` / `forget`, or upgrade tier |
| Search always empty | wrong `projectId` scope | check `switch-project`; try `userId` only |
| Hosted calls 401 | bad/missing `MEMMESH_API_KEY` | re-set key in `config.toml` |

## 4. Report

State clearly what works and what doesn't — don't claim healthy if the
round-trip didn't complete.
