---
name: pin
description: >
  Protect a critical MemMesh memory from consolidation/pruning by raising its
  importance and marking it high-impact — or unpin to release it. Use for
  architecture decisions, security constraints, or immutable team conventions
  that must never be retired by a `dream` pass.
---

# pin

Mark a memory as protected. MemMesh has no separate "pinned" flag — pinning is
expressed through **importance + impact + confirmation**, which the `dream`
consolidation pass treats as do-not-retire.

## Pin

1. Find the item (`memory_search`) and read it (`memory_recall`).
2. Re-assert it at maximum durability with `memory_save` (upsert on the same id):

```jsonc
{ "name": "memory_save",
  "arguments": { "id": "<same id>", "platformId": "local", "scope": "project",
                 "type": "rule", "content": "<verbatim content>",
                 "importance": 10, "metadata": { "pinned": true, "impact": "HIGH" } } }
```

Setting `importance: 10` and `metadata.pinned: true` is the signal the `dream`
skill checks before deleting/superseding anything.

## Unpin

Re-save the same id with `importance` back to a normal value (≈5) and
`metadata.pinned: false`. The item stays in memory but becomes eligible for
consolidation again.

## When to pin

Architecture decisions, security constraints, compliance rules, immutable
conventions — anything where losing it silently would cause real harm. Don't pin
routine preferences; let the engine manage those.
