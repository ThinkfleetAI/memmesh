---
name: memmesh
description: Persistent hierarchical memory shared across every AI tool the user runs. The engine decides what's worth saving — your job is just to feed it raw text via `memory.observe` and to recall via `memory.search` when context would help. Use both on every session.
---

# ThinkFleet Memory — Engine-Side Filtering

You have access to a persistent memory system via the `memmesh` MCP server. **The engine decides what to save.** You just feed it raw text.

This is the opposite of how some memory systems work, where the agent has to judge "is this worth saving?" — that approach fails because judgment varies session to session. Here, you call `memory.observe` with the user's raw message and the engine runs deterministic extraction (regex + structural rules + optional LLM refinement) to find anything memorable.

---

## The two rules

### Rule 1 — RECALL at the start of every session

Before your first substantive response, call `memory.search` to load relevant context:

```jsonc
{ "name": "memory.search",
  "arguments": { "projectId": "<current project>", "limit": 20 } }
```

If you don't know the project, omit `projectId` — search by `userId` instead.

Skip recall only on pure pleasantries ("hi", "thanks"). The moment the user says anything substantive, search first.

### Rule 2 — OBSERVE every user message

After every user message, call `memory.observe` with the raw text:

```jsonc
{ "name": "memory.observe",
  "arguments": {
    "text": "<the user's exact message>",
    "role": "user",
    "projectId": "<current project, if any>",
    "userId": "<current OS user>"
  } }
```

**Don't filter. Don't ask "should this be saved?" Just send the text.** The engine returns the list of items it saved (may be empty for filler — that's fine, you don't have to do anything with the response).

`memory.observe` is cheap (heuristic-only by default), idempotent (re-observing the same text is a no-op for duplicates), and silent on filler.

---

## When to use `memory.save` (rare)

`memory.observe` is your primary tool. `memory.save` is only for the unusual case where you know *exactly* what to save and want to bypass the extractor — e.g., the user explicitly says *"please save the following note verbatim: ..."*.

If you find yourself reaching for `memory.save` to "save what the user just said," that's a sign you should be using `memory.observe` instead.

---

## Scope, citations, recovery

**Scope** the engine picks defaults; you can override in the call when you have better context:

| Scope | When |
|---|---|
| `user` | Personal preferences / identity (default for individual facts) |
| `project` | A specific project's rules / decisions / facts |
| `agent` / `session` / `location` / `platform` | Rarely set explicitly |

**Citations** — when a recalled memory informs your response, mention it briefly so the user can correct stale info:

> "Based on a saved preference (Vitest over Jest), I'll write the test using Vitest's `expect`."

**Corrections** — if the user contradicts a recalled memory ("actually we switched to Jest"), just call `memory.observe` with the new statement. The engine handles supersession.

---

## Defaults for IDs

When you don't have explicit values:

- `platformId`: `"local"` (single-machine default)
- `userId`: `$USER` (OS username)
- `projectId`: git repo directory name, or `null`

---

## Working principle

The point of this system is that **the user never has to repeat themselves**, in any AI tool. Observe everything; recall proactively; cite what you used. The engine handles the rest.
