# MemMesh Desktop — OSS & Enterprise (single source of truth)

Two desktop tiers, one codebase, plus a hub. The desktop is where AI agents run
*and* where governance is enforced; the hub is where memory federates and IT
manages the fleet.

## Two tiers (one shell, license-gated)

| | **OSS Desktop** (built: `crates/desktop`) | **Enterprise Desktop** |
|---|---|---|
| Engine | `memmesh` — heuristic extraction, SQLite, in-process | `thinkfleet-memory-engine` — LLM extraction, **predictions, behaviors**, stronger reranker/recall |
| Console | embedded OSS console | enterprise console (+ Predictions / Behaviors / Compliance tabs) |
| Data | local only | + **phone-home** (two streams below) |
| Compliance | vault + PreToolUse secret-guard | + redact-before-upload, hash-chained boundary audit, admin policy |
| Gate | free | `memmesh license activate` unlocks engine + phone-home + enterprise tabs |

At launch the desktop reads the license → OSS mode (local engine + OSS console)
or Enterprise mode (full engine + enterprise console + phone-home). Same binary
family; no fork. The wry/tao native window is identical — only the engine it
points at, the console it loads, and the capabilities differ.

## Phone-home — TWO separate streams

### 1. Memory stream — "what the org knows" (content, governed)
- **Context** every capture carries: `{ project, position/role }`. Set by the
  user (a selector) and/or pushed by `workstation-registrar` from the SaaS
  (which projects/role this employee has).
- **Selective**: only shareable scopes (`project`/`platform`) sync; **personal
  (`user` scope) never leaves the machine.**
- **Redacted**: PII/secrets stripped at the edge before upload.
- **Aggregated at the hub by `{position, project}`** → org learning: "everything
  we know about Project X"; "how the *Solutions Engineer* role operates." New
  hires/teams inherit it. This is the federation flywheel.

### 2. Utilization stream — "how AI is being used" (metadata, NOT content)
Per user / position / project / workstation, **metadata only**:
- which AI tools/agents ran (Claude Code, Codex, Cursor…), session count + duration
- which models; token & cost
- memory activity volume (observes, recalls) — counts, not text
- **secret-access events** (which secret, which tool, when — never the value)
- tool-call **categories** (e.g. "ran shell", "edited file") — not the command content
- outcomes/adoption signals

Powers: **manage-AI fleet dashboard** (adoption, usage, cost per team/position/
project), **compliance audit**, **ROI**, and **billing/metering**. Emitted by the
desktop (`ai-cli-runner` + MCP + hooks), batched by the edge, sent on the
existing `workstation-registrar` heartbeat channel.

**The privacy line:** utilization = metadata (governable, always safe to send);
memory = content (scoped, redacted, personal-local). Never mix them.

## Manage-AI control plane (the hub admin)
Consumes both streams:
- **Fleet visibility**: every workstation, agent run, tool, memory & secret access.
- **Policy push** the desktop enforces: allowed tools/MCP, which scopes sync,
  block-secrets-in-commands (the guard), kill switch.
- **Compliance**: boundary audit, DSR export/forget, data residency (on-prem engine).

## Aggregation keys & privacy summary
- Privacy boundary = **scope** (`user` local; `project`/`platform` → hub).
- Memory aggregation key = **`{position, project}`**.
- Utilization is metadata only, attributable to user/position/project/workstation.

## Build milestones
1. **License-gated OSS/Enterprise desktop** + hub config + **context selector
   (project/position)**. Makes the two apps physically exist and carry context.
2. **Wrap the enterprise engine** with the console REST (`/vault`, `/stats`,
   `/memory`, `/graph`, predictions/behaviors) so Enterprise runs on the full
   engine.
3. **Phone-home** — both streams: memory (scoped, redacted, `{position,project}`)
   + utilization (metadata) on the heartbeat channel.
4. **Manage-AI fleet console** at the hub — visibility + policy + compliance.
5. **Federation** — provenance/CRDT merge → org memory graph → behaviors/
   predictions across people/teams.

## What exists today
- ✅ OSS desktop (`crates/desktop`) — engine + console in a native window.
- ✅ Vault (typed forms, execute-through-vault, scrub) + PreToolUse secret-guard.
- ✅ Sync engine (LWW), scope model, `workstation-registrar` heartbeat (phone-home channel).
- ✅ Enterprise engine predictions/behaviors backends (gRPC) + SaaS console tabs.
- ◻︎ position/role dimension; redact-before-upload; utilization metering; fleet console; CRDT/federation.
