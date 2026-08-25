# MemMesh Enterprise Edge — enforce, phone-home, federate

Design spec for the enterprise deployment: a **local edge** on every user's
machine that governs what leaves the device, streams governed memory + usage to
a **hub** (SaaS or on-prem), where the full engine federates individual work
into org-wide learning.

## Two engines, one product ladder
- **OSS `memmesh`** — the *individual* on-ramp. Single binary, SQLite, heuristic
  extraction, stdio-MCP + console. Free, fully local. (This repo.)
- **Enterprise engine** — `thinkfleet-memory-engine` (gRPC): LLM extraction, KG,
  predictions/behaviors, compliance packs, brainId tenant isolation. Runs as
  SaaS **or on-prem**. The enterprise never runs the OSS engine.
- **The edge** — `memmesh-proxy` ("MemMesh Edge: streams conversations into the
  memory engine and phones home usage counts only"). The local client for
  enterprise: it does NOT hold the brain; it governs and forwards.

## Responsibility split

### Local edge (per user/machine) — the trust boundary
Everything that must be true *before data leaves the device*:
1. **Capture** — observe conversations/tool activity from every AI tool
   (Claude Code, Codex, Cursor, ChatGPT desktop) via MCP + hooks.
2. **Enforce (deterministic, not model-dependent):**
   - **Secret-guard** (shipped): PreToolUse hook blocks any command carrying a
     live credential → redirect to the vault. Secrets can't reach a shell
     command or transcript.
   - **Vault**: credentials entered by the human (never in chat); the AI
     references `{{memmesh:name}}` and runs via execute-through-vault; values
     scrubbed from output. AI never sees plaintext.
   - **Redact**: strip PII/secrets from text before it streams to the hub
     (extend the `contains_secret` guard + a PII classifier).
3. **Selective sync (scope = privacy policy):** `user`-scope stays local;
   `project`/`platform`-scope syncs to the hub. Explicit, auditable, opt-in per
   scope. "This never leaves my machine" is enforceable.
4. **Local audit** — tamper-evident (hash-chained) log of exactly what crossed
   the boundary: memories synced, secrets accessed (by which tool, when),
   recalls served. IT-inspectable.
5. **Phone-home (metering + monitoring):** usage counts, secret-access events,
   recall activity → hub. Billing signal + the compliance audit trail. No
   memory *content* beyond what selective-sync already governs.

### Hub (SaaS / on-prem engine) — the brain
- Extraction, KG, consolidation, **behaviors/predictions**, tenant isolation.
- **Federation**: individual jobs → org memory graph → cross-enterprise
  learning; new employees' AIs inherit collective experience.
- Admin console: dashboard, memories, graph, ideas, secrets, predictions,
  behaviors, compliance, exports, activity.

## The compliance contract (what an enterprise buys)
- **Data residency**: on-prem option → memory + secrets never leave the network.
- **Boundary audit**: every byte that crosses machine→hub is logged and
  attributable (tool, user, scope, timestamp).
- **Credential isolation**: the AI provably never receives plaintext secrets
  (guard + vault + scrub), and every secret access is grant-logged (the engine's
  time-boxed broker already models this).
- **DSR**: export / forget from the console; consent records honored before
  mining (the `CONSENT` memory type).
- **Redaction-before-upload**: PII/secrets filtered at the edge.

## Sync/merge — the open engineering gap
Current sync is **LWW** (last-write-wins). Org federation needs:
- **CRDT or provenance-precedence merge** (source-of-record wins, not clock).
- **ACL mirroring** — org memory respects source-system permissions.
- **Federation / Interchange Protocol** — compose many brains into a super-brain
  (staged; prove single-brain value first).

## Status
- ✅ Vault (encrypted, keychain, execute-through-vault, scrubbed) — OSS + SaaS.
- ✅ Secret-guard PreToolUse enforcement — OSS edge/console install.
- ✅ Sync engine (LWW) + scope model.
- ◻︎ PII classifier / redact-before-upload.
- ◻︎ Phone-home metering channel (usage + secret-access + recall) → hub.
- ◻︎ Local hash-chained boundary audit surfaced in the console.
- ◻︎ CRDT/provenance merge; ACL mirroring; federation.

## Phases
1. **Governed edge v1** — guard (done) + vault (done) + redact + local audit +
   scope-based selective sync.
2. **Phone-home** — metering + secret-access + recall telemetry to the hub;
   console "Activity/Audit" tab.
3. **Federation** — provenance merge → org graph → behaviors/predictions across
   people/teams.
