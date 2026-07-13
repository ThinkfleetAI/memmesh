---
name: onboard
description: >
  Set up MemMesh for a new project — verify the MCP server is wired, pick
  local vs hosted, import any existing project knowledge (MEMORY.md, CLAUDE.md,
  a mem0 export), and seed initial scopes. Use on first run in a repo, when the
  API key changes, or to re-run setup after config changes.
---

# onboard

Get a project ready to use MemMesh.

## 1. Verify wiring
```bash
memmesh doctor        # binary + MCP config + skill presence
```
If the MCP server isn't connected, run `npx @thinkfleet/memmesh install` (see the
`memmesh-cli` skill), then re-check.

## 2. Local or hosted?

- **Local** (default): SQLite, no key, offline. Good for dev tools.
- **Hosted**: set `MEMMESH_API_KEY` / `sync.url` in
  `~/.thinkfleet-memory/config.toml`. Needed for cross-device sync, the full SDK,
  and server-side prediction/verticals.

## 3. Seed existing knowledge

If the repo already has durable context, import it (see the `import` skill):
`MEMORY.md`, `CLAUDE.md`, an ADR folder, or a **mem0 export** (then consider
`memmesh-migrate` for a full switch). Feed each item via `memory_observe` so the
engine extracts + builds the graph, scoped `project`.

## 4. Confirm

Run `stats` to show what's now in scope, and remind the user of the two-rule
loop: the agent will **observe** their messages and **recall** at session start
automatically (the always-on `memmesh` skill). Nothing else to configure.
