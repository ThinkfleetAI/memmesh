---
name: memmesh
description: Persistent hierarchical memory shared across every AI tool the user runs. The engine decides what's worth saving — your job is just to feed it raw text via `memory.observe` and to recall via `memory.search` when context would help. Use both on every session.
---

# ThinkFleet Memory — Engine-Side Filtering

You have access to a persistent memory system via the `memmesh` MCP server. **The engine decides what to save.** You just feed it raw text.

This is the opposite of how some memory systems work, where the agent has to judge "is this worth saving?" — that approach fails because judgment varies session to session. Here, you call `memory.observe` with the user's raw message and the engine runs deterministic extraction (regex + structural rules + optional LLM refinement) to find anything memorable.

---

## The two rules

### Rule 1 — RECALL at the start of every session

Before your first substantive response, call `memory.search` to load relevant context:

```jsonc
{ "name": "memory.search",
  "arguments": { "projectId": "<current project>", "limit": 20 } }
```

If you don't know the project, omit `projectId` — search by `userId` instead.

Skip recall only on pure pleasantries ("hi", "thanks"). The moment the user says anything substantive, search first.

### Rule 2 — OBSERVE every user message

After every user message, call `memory.observe` with the raw text:

```jsonc
{ "name": "memory.observe",
  "arguments": {
    "text": "<the user's exact message>",
    "role": "user",
    "projectId": "<current project, if any>",
    "userId": "<current OS user>"
  } }
```

**Don't filter. Don't ask "should this be saved?" Just send the text.** The engine returns the list of items it saved (may be empty for filler — that's fine, you don't have to do anything with the response).

`memory.observe` is cheap (heuristic-only by default), idempotent (re-observing the same text is a no-op for duplicates), and silent on filler.

---

## When to use `memory.save` (rare)

`memory.observe` is your primary tool. `memory.save` is only for the unusual case where you know *exactly* what to save and want to bypass the extractor — e.g., the user explicitly says *"please save the following note verbatim: ..."*.

If you find yourself reaching for `memory.save` to "save what the user just said," that's a sign you should be using `memory.observe` instead.

---

## Credentials & secrets — ALWAYS use the vault, never the chat

When a task needs a credential — an API key, password, token, or connection
string — **do NOT ask the user to type or paste it into the conversation, and
never write the raw value into a command.** Chat and transcripts are not a safe
place for secrets. Route it through the encrypted vault instead:

1. **Request it.** Call `memory_secret_request` with a stable `name` and a short
   `purpose`:

   ```jsonc
   { "name": "memory_secret_request",
     "arguments": { "name": "aws-prod", "purpose": "list S3 buckets" } }
   ```

   - If it reports the secret is **missing**, it returns a one-click link that
     opens the vault with the name pre-filled (e.g.
     `http://127.0.0.1:7878/?tab=vault&add=aws-prod`). **Relay that link to the
     user and ask them to enter the value there** (or tell them to run
     `memmesh secret set aws-prod`). Then wait — do not proceed until it exists.
   - If it's **available**, go straight to step 2.

2. **Use it without seeing it.** Reference the secret by name as
   `{{memmesh:NAME}}` inside a command and run it through `memory_secret_run`:

   ```jsonc
   { "name": "memory_secret_run",
     "arguments": { "command": "aws s3 ls --profile {{memmesh:aws-prod}}" } }
   ```

   The engine substitutes the real value **inside its own process**, runs the
   command, and returns output with every secret value scrubbed to
   `[redacted]`. You never receive the plaintext — so never try to `echo` or
   print a secret to "read" it; that returns `[redacted]`.

Use `memory_secret_list` to see which secrets already exist (names only, never
values). **Rule of thumb:** the moment you're about to say "please paste your
API key / password," stop and call `memory_secret_request` instead.

---

## Scope, citations, recovery

**Scope** the engine picks defaults; you can override in the call when you have better context:

| Scope | When |
|---|---|
| `user` | Personal preferences / identity (default for individual facts) |
| `project` | A specific project's rules / decisions / facts |
| `agent` / `session` / `location` / `platform` | Rarely set explicitly |

**Citations** — when a recalled memory informs your response, mention it briefly so the user can correct stale info:

> "Based on a saved preference (Vitest over Jest), I'll write the test using Vitest's `expect`."

**Corrections** — if the user contradicts a recalled memory ("actually we switched to Jest"), just call `memory.observe` with the new statement. The engine handles supersession.

---

## Defaults for IDs

When you don't have explicit values:

- `platformId`: `"local"` (single-machine default)
- `userId`: `$USER` (OS username)
- `projectId`: git repo directory name, or `null`

---

## Working principle

The point of this system is that **the user never has to repeat themselves**, in any AI tool. Observe everything; recall proactively; cite what you used. The engine handles the rest.
