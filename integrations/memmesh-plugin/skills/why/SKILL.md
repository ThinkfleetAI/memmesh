---
name: why
description: >
  Explain a MemMesh prediction or recalled fact — surface its provenance
  (evidence memories), its calibrated confidence, and whether the model abstained
  and why. Use when the user asks "why do you think that", "what's this based on",
  "how sure are you", or needs an auditable, defensible answer for a regulated
  decision.
---

# why

Make MemMesh's outputs auditable. Every prediction and consolidated fact carries
provenance and a calibrated confidence — this skill exposes them so a human can
check the reasoning.

## Provenance — what is this based on?

A prediction (from `predict` / `memory_build_context`) returns evidence memory
ids. Resolve each to its content:
```jsonc
{ "name": "memory_recall", "arguments": { "id": "<evidence id>" } }
```
List the actual memories that drove the conclusion. If a fact was consolidated,
its superseded ancestors show the history — that's the audit trail.

## Calibration — is the confidence trustworthy?

MemMesh confidences are calibrated: 0.8 should be right ~80% of the time. To show
the reliability curve (predicted vs. observed), use the hosted SDK:
```ts
const cal = await memory.lattice.getCalibration({ subjectKind: "user" });
```
Report the calibration error alongside the confidence, so "80%" is backed by
evidence it *means* 80%.

## Abstention — the honest "I don't know yet"

If a prediction abstained, explain the reason (insufficient/contradictory
evidence, subject too new). Frame abstention as a **feature**: MemMesh declines
rather than fabricate a confident-looking number. This is what makes it usable
for EU AI Act / regulated decisions where a wrong confident answer is worse than
no answer.

## For regulated use

Pair this with the SDK's `compliance.listAuditEvents` / `exportSubject` to
produce a full defensible record of what was known, when, and what drove a
decision.
