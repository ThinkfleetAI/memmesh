# MemMesh plugin for Claude Code

Persistent memory **and calibrated prediction** for your AI agent. MemMesh
remembers decisions, preferences, and patterns across sessions, links them into a
bi-temporal knowledge graph, and forecasts what a subject will do next — with
provenance and honest abstention.

## Install

```bash
# Add the marketplace, then install the plugin:
/plugin marketplace add ThinkfleetAI/memmesh
/plugin install memmesh

# Or wire the MCP server + skills directly (local, no account):
npx @thinkfleet/memmesh install
```

Local mode runs fully offline over SQLite and needs **no API key**. For hosted
mode (cross-device sync, full SDK, server-side prediction/verticals), set
`MEMMESH_API_KEY` — see the `memmesh-sdk` skill.

## Skills in this plugin

The agent invokes these by name; the MCP tools they call appear to Claude Code as
`mcp__memmesh__memory_*`.

**Everyday memory**
- `remember` — save what the user asks to keep (observe / verbatim)
- `forget` — delete or correct a memory (confirm first; soft vs hard)
- `peek` — quick one-liner lookup or fetch-by-id
- `tour` — browse everything, grouped by type
- `stats` — counts by type/scope/status + age span
- `context-loader` — load relevant context before a task (incl. subject bundle)
- `dream` — consolidate duplicates/contradictions (respects pins)
- `pin` — protect a critical memory from consolidation
- `export` / `import` — portable backup, restore, or seed from MEMORY.md / mem0
- `onboard` — set up MemMesh in a new project
- `switch-project` — target another project scope or widen the search
- `health` — diagnose connectivity + read/write round-trip

**Prediction & graph (what a plain memory layer can't do)**
- `predict` — forecast a subject's next move, calibrated + with provenance
- `why` — explain a prediction: evidence, calibration, and abstention
- `behaviors` — surface emergent, mined behavior patterns
- `graph` — multi-hop reasoning + point-in-time + anticipatory retrieval
- `benchmark` — run the LOCOMO/BEAM harness head-to-head vs Mem0 / Zep

## Standalone skills (outside the plugin)

For SDK/CLI reference and repo integration, see the top-level [`skills/`](../../skills):
`memmesh-sdk`, `memmesh-cli`, `memmesh-integrate`, `memmesh-test-integration`,
`memmesh-migrate`.

## The always-on loop

The bundled `memmesh` skill runs the core loop automatically: **observe** every
user message (the engine decides what to save) and **recall** at session start.
You don't have to trigger it.
