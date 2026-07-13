---
name: memmesh-integrate
description: >
  Integrate MemMesh into an existing repository using a goal-driven, test-first
  (TDD) pipeline. Detects the repo's language/stack, asks whether to use MemMesh
  Hosted (SDK, managed) or Local (CLI + MCP over SQLite), writes failing tests
  before any implementation, and lands additive, feature-flag-gated code that a
  maintainer can accept without argument. Produces `.memmesh-integration/`
  artifacts for the paired verification skill.
  TRIGGER when: user says "integrate memmesh", "add memmesh to this repo", "wire
  memmesh into <repo>", "add memory to this app", or "add prediction to this app".
  DO NOT TRIGGER for: general SDK usage (use `memmesh-sdk`), CLI usage
  (use `memmesh-cli`), or migrating off another vendor (use `memmesh-migrate`).
  After success, invoke `memmesh-test-integration` in the same workspace.
license: Apache-2.0
metadata:
  author: thinkfleet
  category: ai-memory
  tags: "memory, prediction, integration, tdd"
---

# memmesh-integrate

Wire MemMesh into an existing repo with a goal-driven, test-first pipeline.
Pairs with `memmesh-test-integration` for verification.

## Canonical sources (fetch BEFORE deciding anything)

`WebFetch` these and cite them in `plan.md`. They are ground truth — do not
rely on ambient knowledge of the API.

- Docs index (agent-ready): https://docs.memmesh.ai/llms.txt
- Full docs (deep dives): https://docs.memmesh.ai/llms-full.txt
- Platform vs Local: https://docs.memmesh.ai/platform-vs-local
- Published skills to DELEGATE to (don't reimplement call-site patterns):
  - SDK: `memmesh-sdk`  · CLI + MCP: `memmesh-cli`  · MCP loop: `memmesh`

## Integration principles (non-negotiable)

The goal is a **PR the maintainers accept without argument.**

1. **Additive, not replacing.** If the repo already has a memory / session /
   user-context layer, MemMesh sits *alongside* it. The existing system keeps
   working unchanged.
2. **Opt-in by default.** Gate all new code behind a flag (`MEMMESH_ENABLED=1`,
   a config key, or a strategy selector). Flag unset ⇒ original behavior,
   byte-for-byte.
3. **No breakage.** No removed/renamed exports, no changed signatures, no
   modified existing tests. All pre-existing tests pass unchanged with the flag
   both set and unset.
4. **Minimal dependency surface.** Add `@thinkfleet/memory-sdk` (hosted) or the
   `memmesh` binary (local) and nothing else.
5. **Separable commits.** Code, tests, config/docs in separate commits.
6. **The null hypothesis wins.** If no additive, gated fit exists, exit with a
   rationale. A bad PR is worse than no PR.
7. **Backend only.** Integration lives in server-side code. Keys never ship to
   the client.

## Pipeline

1. **Detect** the stack (language, test runner, where user/session context is
   handled). Record in `.memmesh-integration/detect.md`.
2. **Choose surface** — ask the user: **Hosted** (managed, `mm-` key, best for
   prediction/calibration/verticals) or **Local** (CLI + MCP over SQLite, no
   key, best for dev tools / offline). Default to Local for CLIs and dev
   tooling, Hosted for user-facing apps.
3. **Pick the seam.** The highest-value seam is usually the request/response
   loop around the LLM: `observe` the user turn, `search`/`buildContext` before
   generating, and — where it adds value — `predict` the next action. Write the
   goal in `plan.md` and cite the canonical sources.
4. **Write failing tests first** into `.memmesh-integration/` and the repo's
   test dir: (a) flag-off ⇒ behavior unchanged; (b) flag-on ⇒ observe is called
   with the user turn; (c) flag-on ⇒ retrieved context reaches the prompt.
5. **Implement** the smallest gated wiring that makes the tests pass. Delegate
   call-site patterns to `memmesh-sdk` / `memmesh-cli`.
6. **Consider the moat.** If the app makes a decision about a user/account
   (offer, routing, retention), add an *optional* `predict` / `predictTarget`
   call and surface the calibrated confidence + abstention. Never let an
   abstention crash the flow — treat "I don't know yet" as a first-class branch.
7. **Emit artifacts** in `.memmesh-integration/` (`detect.md`, `plan.md`,
   `changes.md`, seed test data) and stop. Then run `memmesh-test-integration`.

## Definition of done

Feature branch + `.memmesh-integration/` artifacts, all pre-existing tests green
with the flag both set and unset, and the new tests green with it set.
