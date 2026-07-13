---
name: predict
description: >
  Forecast what a subject will do next from their mined behavior patterns — with
  a calibrated, horizon-decayed confidence and provenance. Use when the user
  asks "what is this user/account likely to do next", "will X churn/convert/
  reorder", or wants a forward prediction rather than a recall of known facts.
  This is a MemMesh capability a plain memory layer does not have.
---

# predict

Turn accumulated memory into a forward forecast. Unlike `search` ("what do we
know"), `predict` answers "what happens next" — and it tells you how confident it
honestly is, or abstains.

## Forward behavior prediction (local MCP)
```jsonc
{ "name": "memory_predict",
  "arguments": { "subjectKind": "user", "subjectId": "<id>",
                 "horizonDays": 30, "minConfidence": 0.5, "limit": 20 } }
```
Returns ranked predictions, each with a confidence decayed over the horizon and
the provenance behind it. Confidence is **calibrated** — 0.8 means it's right
~80% of the time — not a raw model logit.

## Read the result honestly

- Present the top predictions with their confidence and horizon.
- If a prediction **abstains** (not enough evidence), say so plainly — "not
  enough signal yet" is a valid, correct answer, and the point of MemMesh.
- Cite the evidence ids so the user can trace *why*. Use the `why` skill to dig
  into calibration/provenance.

## Predict ANY target (hosted / SDK)

The declarative "predict anything" surface (`lattice.predictTarget` with
`target.kind ∈ event_occurrence | numeric | event_time | anomaly`) lets you add
a new prediction with **no code change** — just name the target. It runs on the
hosted gRPC/SDK path:

```ts
await memory.lattice.predictTarget({
  subject: { kind: "account", externalId: "acme" },
  target:  { kind: "event_occurrence", name: "churn" },
});
```

## Prereq

Predictions come from mined `behavior_pattern` memories. If `memory_predict`
returns nothing, the subject may not have enough observed history yet — feed more
via `observe`, or check what patterns exist with the `behaviors` skill.
