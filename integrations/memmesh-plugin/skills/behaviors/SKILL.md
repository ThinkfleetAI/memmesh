---
name: behaviors
description: >
  Surface emergent behavior patterns MemMesh has mined from a subject's history —
  recurring habits nobody predefined, each with prevalence, stability, and the
  evidence behind it. Use when the user asks "what patterns do you see", "what
  are this user's habits", or wants the patterns that drive predictions.
---

# behaviors

Show the patterns MemMesh discovered on its own. These `behavior_pattern`
memories are what `predict` projects forward — inspecting them explains the
forecasts.

## List mined patterns (local MCP)
```jsonc
{ "name": "memory_search",
  "arguments": { "type": "behavior_pattern", "projectId": "<repo>", "limit": 50 } }
```
Or scope to one subject and read them out of the context bundle:
```jsonc
{ "name": "memory_build_context",
  "arguments": { "subjectKind": "user", "subjectId": "<id>", "include": ["patterns"] } }
```

## Discover new patterns (hosted / SDK)

The discovery pass that finds patterns nobody predefined runs on the SDK:
```ts
const behaviors = await memory.behaviors.discover({ projectId: "myapp" });
// each: { pattern, prevalence, stability, evidenceMemoryIds }
```

## Present them

For each pattern show: the behavior, how often it holds (prevalence), how stable
it is over time (stability), and a couple of evidence memories. Rank by
stability × prevalence — the strongest, most reliable habits first.

## Why it matters

A vector-recall memory layer can only return facts you already stated. MemMesh
*derives* structure — "books gym classes on Mondays", "reorders ~every 6 weeks" —
from raw observations. That derived structure is the input to `predict` and the
reason the predictions have provenance.
