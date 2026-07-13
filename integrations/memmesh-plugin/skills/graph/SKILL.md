---
name: graph
description: >
  Query MemMesh's bi-temporal knowledge graph — multi-hop reasoning across
  entities, point-in-time "what did we believe on date X", and anticipatory
  retrieval via spreading activation. Use for questions no single stored fact
  answers, or to see how knowledge about an entity changed over time.
---

# graph

MemMesh links memories into a knowledge graph whose edges are **bi-temporal**
(each has `valid_from` / `valid_to`). That enables answers a flat store can't
give.

## Multi-hop reasoning

Answer questions that require chaining edges — "who acquired the company Sarah
founded":
```jsonc
{ "name": "memory_graph_reason",
  "arguments": { "anchorEntityId": "<entity id>", "maxHops": 3, "maxPaths": 20 } }
```
Returns ranked paths (scored by edge weight × recency). The anchor is an entity
id — resolve names to ids via a graph query first.

## Point-in-time — what did we believe then?

```jsonc
{ "name": "memory_query_graph",
  "arguments": { "subjectId": "<entity id>", "asOf": "2026-01-01T00:00:00Z" } }
```
Omit `asOf` for the current view. This reconstructs the graph as it stood on any
date — the bi-temporal record, not just the latest state.

## Anticipatory retrieval (spreading activation)

Given the memories a session is working with, surface what's most likely needed
next:
```jsonc
{ "name": "memory_prefetch_related", "arguments": { "seedMemoryIds": ["<id>","<id>"], "limit": 10 } }
```

## Building the graph

Edges come from client-LLM extraction — the engine hands you a prompt, your own
model extracts entities/edges, you commit them (zero engine-side LLM cost):
```jsonc
{ "name": "memory_extract_pending", "arguments": { "projectId": "<repo>", "limit": 10 } }
// run each prompt through your model, then:
{ "name": "memory_commit_extraction", "arguments": { "memoryId": "…", "contentHash": "…", "entities": [...], "edges": [...] } }
```
Run this loop until `extract_pending` returns empty to fully populate the graph
for reasoning.
