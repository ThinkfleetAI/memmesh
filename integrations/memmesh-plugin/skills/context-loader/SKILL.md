---
name: context-loader
description: >
  Load relevant MemMesh context before starting work — searches memory and, for
  a specific subject, assembles a token-budgeted bundle (profile + behavior
  patterns + forward predictions + top memories) in one call. Use when beginning
  a task, switching context, or when project history / past decisions / a
  subject's profile would help.
---

# context-loader

> **⚙️ Requires MemMesh hosted mode.** Calibrated prediction and behavior discovery run on the hosted engine — set your `mm-` API key. On a local / open-source install these tools (`memory_predict`, `memory_build_context`) are not registered; if a call returns "unknown tool", tell the user this is a hosted capability and fall back to `search` / `recall` for what's already known.


Prime the session with the right memory before you act.

## General project/session context
```jsonc
{ "name": "memory_search", "arguments": { "projectId": "<repo>", "limit": 20 } }
```
Skip only on pure pleasantries; the moment the task is substantive, load first.

## A specific subject — the synthesized bundle

When you're about to reason about one entity (a user, contact, account), don't
fire five searches — get the assembled picture in one call:

```jsonc
{ "name": "memory_build_context",
  "arguments": { "subjectKind": "user", "subjectId": "<id>", "maxTokens": 2000,
                 "include": ["profile","patterns","predictions","memories"] } }
```

This returns the profile, active behavior patterns, **forward predictions**, and
top memories — with provenance ids — ready to drop at the top of your prompt.
That prediction section is the differentiator: you enter the task already knowing
what the subject is likely to do next.

## Anticipatory follow-on

Once you're working with a few memories, pull what's most likely needed next via
spreading activation over the graph:

```jsonc
{ "name": "memory_prefetch_related", "arguments": { "seedMemoryIds": ["<id1>","<id2>"], "limit": 10 } }
```

## Cite what you use

When a loaded memory shapes your response, mention it briefly so the user can
correct stale info.
