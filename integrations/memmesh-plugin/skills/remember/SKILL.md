---
name: remember
description: >
  Store a memory in MemMesh from the user's input. Prefers memory_observe (the
  engine decides what to extract) and falls back to memory_save only for
  verbatim/structured notes. Use when the user says "remember this", "save this",
  "note that", "from now on", "we decided", or explicitly asks to record a
  decision, preference, convention, or learning.
---

# remember

Persist something the user asked you to keep.

## Default: observe (let the engine extract)

For almost everything, call **`memory_observe`** with the user's raw statement.
The engine runs deterministic extraction and saves what's memorable — you don't
judge.

```jsonc
{ "name": "memory_observe",
  "arguments": { "text": "<the user's exact statement>", "role": "user",
                 "projectId": "<git repo name or null>", "userId": "<OS user>" } }
```

## Verbatim: save (rare)

Only when the user says "save this exactly / verbatim" or you have structured
data the extractor would mangle, use **`memory_save`** with an explicit id,
type, and scope:

```jsonc
{ "name": "memory_save",
  "arguments": { "id": "<21+ char id>", "platformId": "local", "type": "preference",
                 "scope": "user", "content": "prefers pnpm over npm" } }
```

Pick scope: `user` (personal preference/identity), `project` (a repo's rule/
decision/fact), else let the engine default.

## After saving

Briefly confirm what was stored and at which scope, so the user can correct it.
If they're *changing* an existing fact, see the `pin`/correction note: observe
the new statement and let the engine supersede — don't delete.
