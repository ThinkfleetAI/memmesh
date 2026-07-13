---
name: memmesh-test-integration
description: >
  Verify a MemMesh integration produced by memmesh-integrate. Runs in the same
  workspace: executes the repo's native test suite, then exercises a real
  end-to-end smoke flow (observe → search → optionally predict) against the
  user's live key or local engine, and produces a pass/fail scorecard.
  TRIGGER when: the user has just run memmesh-integrate and says "verify",
  "test the integration", or when a `.memmesh-integration/` directory exists and
  tests have not been run yet.
  DO NOT TRIGGER for: first-time wiring (use memmesh-integrate) or vendor
  migration (use memmesh-migrate).
license: Apache-2.0
metadata:
  author: thinkfleet
  category: ai-memory
  tags: "memory, integration, testing, verification"
---

# memmesh-test-integration

Prove the integration actually works — not just that it typechecks.

## Preconditions

- A `.memmesh-integration/` directory exists (else tell the user to run
  `memmesh-integrate` first).
- Credentials: Hosted ⇒ `MEMMESH_API_KEY` set; Local ⇒ `memmesh doctor` passes.

## Steps

1. **Static gate.** Typecheck / lint / build. Any failure ⇒ stop, report.
2. **Native suite.** Run the repo's own tests **twice** — once with
   `MEMMESH_ENABLED` unset (must be byte-for-byte the original behavior) and
   once set. Both must pass.
3. **Real E2E smoke** against the live engine (not a mock):
   - `observe` a known fact for a throwaway `userId` (e.g. `smoke-<runid>`).
   - `search` for it; assert the fact comes back.
   - `buildContext` for the subject; assert the fact appears in the bundle.
   - If prediction was wired: call `predict` / `predictTarget` and assert you get
     *either* a calibrated probability *or* an honest abstention — both are a
     pass; a crash or an uncalibrated 1.0/0.0 with no evidence is a fail.
   - Clean up: `memory_delete` (or `compliance.hardDeleteSubject`) the throwaway
     subject so the smoke run leaves no residue.
4. **Scorecard.** Write `.memmesh-integration/scorecard.md`:

   | Check | Result |
   |---|---|
   | Typecheck / build | ✅ / ❌ |
   | Native tests (flag off) | ✅ / ❌ |
   | Native tests (flag on) | ✅ / ❌ |
   | E2E observe→search | ✅ / ❌ |
   | E2E buildContext | ✅ / ❌ |
   | E2E predict (calibrated OR abstained) | ✅ / ❌ / n/a |
   | Smoke cleanup | ✅ / ❌ |

## Definition of done

Every applicable row green, throwaway data removed, and a one-paragraph verdict:
ship / needs-work, with the failing checks called out.
