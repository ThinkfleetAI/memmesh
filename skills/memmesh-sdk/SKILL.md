---
name: memmesh-sdk
description: >
  MemMesh TypeScript SDK reference (@thinkfleet/memory-sdk) for the hosted
  platform at app.memmesh.ai. Covers the ThinkFleetMemory client — observe /
  search / list, the predict + lattice prediction surface, closed-loop
  learning (recordDecision / recordOutcome), emergent behavior discovery, and
  the health / financial vertical packs.
  TRIGGER when: user is writing code that calls the MemMesh SDK, mentions
  "@thinkfleet/memory-sdk", "ThinkFleetMemory", "memmesh sdk", "lattice.predict",
  "predictTarget", or wants to add memory OR prediction to a TS/JS app.
  DO NOT TRIGGER for: the local MCP observe/recall loop (that's the always-on
  `memmesh` skill), CLI usage (use `memmesh-cli`), or wiring into an existing
  repo (use `memmesh-integrate`).
license: Apache-2.0
metadata:
  author: thinkfleet
  category: ai-memory
  tags: "memory, prediction, calibration, typescript, knowledge-graph"
compatibility: Requires Node.js 18+. npm install @thinkfleet/memory-sdk. A MEMMESH_API_KEY (hosted) or a Cognito JWT. For a no-key local setup use the memmesh CLI + MCP instead.
---

# MemMesh TypeScript SDK

MemMesh is not just a store-and-recall memory layer. It is a **memory +
calibrated-prediction + behavior-discovery engine** over a bi-temporal
knowledge graph. The SDK talks to the hosted platform (`app.memmesh.ai`) over
REST; for a zero-infra local setup, drive the same engine through the CLI +
MCP server instead (see `memmesh-cli`).

> **Mental model:** `observe` (feed raw text — the engine decides what to save)
> → `search` / `buildContext` (retrieve) → `predict` (forecast the subject's
> next move, with a calibrated confidence and provenance).

## Step 1 — install and authenticate

```bash
npm install @thinkfleet/memory-sdk
export MEMMESH_API_KEY="mm-your-api-key"   # from app.memmesh.ai
```

## Step 2 — initialize

```ts
import { ThinkFleetMemory } from "@thinkfleet/memory-sdk";

const memory = new ThinkFleetMemory({
  apiKey: process.env.MEMMESH_API_KEY,      // or a Cognito JWT via `token`
  // baseUrl defaults to https://app.memmesh.ai
});
```

## Step 3 — the core loop: observe → retrieve → (predict)

### Observe — the engine decides what to save
Unlike layers where *you* judge "is this worth saving?", you feed MemMesh raw
text and its extractor (regex + structural rules + optional LLM refinement)
decides. Cheap, idempotent, silent on filler.

```ts
await memory.memory.observe({
  text: "Alice is vegetarian and allergic to nuts. She books gym classes on Mondays.",
  userId: "alice",
  projectId: "myapp",
});
```

There are also typed intake helpers: `observeImage`, `observeVoice`,
`observeDocument`, `ingestMedia`.

### Retrieve — search or a full context bundle
```ts
const hits = await memory.memory.search({ query: "dietary restrictions", userId: "alice" });

// Or the synthesized, token-budgeted bundle (profile + patterns + predictions + top memories):
const ctx = await memory.context.build({ subjectKind: "user", subjectId: "alice", maxTokens: 2000 });
```

## The moat — predict anything, with calibration + abstention

This is what a vector-recall layer cannot do. Predictions carry a **calibrated**
confidence ("80% means 80%"), **provenance** (`evidenceMemoryIds`), and a
first-class **abstention** ("I don't know yet" is a valid, honest answer).

```ts
// Forward behavior prediction — what will this subject do next?
const preds = await memory.lattice.predict({ subjectKind: "user", subjectId: "alice", horizonDays: 30 });

// Declarative "predict ANY target" — no code change to add a new prediction:
const p = await memory.lattice.predictTarget({
  subject: { kind: "user", externalId: "alice" },
  target: { kind: "event_occurrence", name: "churn" },   // or numeric | event_time | anomaly
});
if (p.abstained) {
  console.log("abstained:", p.abstentionReason);         // honest "not enough evidence"
} else {
  console.log(p.probability, "±", p.calibration, "because", p.evidenceMemoryIds);
}

// Is the model actually calibrated? Check the reliability curve:
const cal = await memory.lattice.getCalibration({ subjectKind: "user" });
```

## Closed-loop learning — make predictions get better

Record the decision you made and the outcome that followed; the engine feeds
that back into calibration and effectiveness reporting.

```ts
const d = await memory.learning.recordDecision({ subjectId: "alice", decision: "sent_winback_offer" });
await memory.learning.recordOutcome({ decisionId: d.id, outcome: "converted", value: 49.0 });
const eff = await memory.learning.getEffectiveness({ subjectKind: "user" });
```

## Emergent behavior discovery — patterns nobody predefined

```ts
const behaviors = await memory.behaviors.discover({ projectId: "myapp" });
// each carries prevalence, stability, and the evidence memories behind it
```

## Knowledge graph (bi-temporal)

```ts
const g = await memory.context.queryGraph({ subjectId: "alice", asOf: "2026-01-01T00:00:00Z" });
// "what did we believe about Alice on Jan 1" — every edge has valid_from / valid_to
```

## Vertical packs

```ts
// Health
await memory.health.recordBiomarker({ subjectId: "alice", marker: "hba1c", value: 5.4 });
const risk = await memory.health.getCohortRisk({ condition: "prediabetes" });

// Financial
await memory.financial.ingestPrices({ symbol: "AAPL", bars: [...] });
const f = await memory.financial.predict({ symbol: "AAPL", target: { kind: "numeric", name: "close_5d" } });
```

## Compliance & consent (regulated use)

```ts
await memory.consent.optOut({ subjectId: "alice" });
await memory.compliance.hardDeleteSubject({ subjectId: "alice" });   // GDPR right-to-forget
const audit = await memory.compliance.listAuditEvents({ subjectId: "alice" });
```

## Scoping model

Six-level hierarchy: `platform` › `project` › `location` › `agent` › `user` ›
`session`. Pass `projectId` / `userId` / `agentId` / `sessionId` to scope any
call. Lifecycle: `pending → confirmed → superseded → rejected` (the engine
supersedes on contradiction — you don't hand-manage it).

## Language support

TypeScript/JavaScript is the shipping distributed SDK today. For non-TS stacks,
use the **MCP server** (any MCP-capable agent) or the REST API directly
(`llms.txt` / OpenAPI at docs.memmesh.ai). A Python SDK is on the roadmap.

## Ground truth (fetch before relying on ambient knowledge)

- Docs index (agent-ready): https://docs.memmesh.ai/llms.txt
- SDK examples: `predict-anything.ts`, `financial-demo.ts`, `next-best-offer.ts`
- Related skills: `memmesh` (MCP loop), `memmesh-cli`, `memmesh-integrate`
