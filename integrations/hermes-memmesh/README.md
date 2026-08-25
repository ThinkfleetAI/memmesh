# MemMesh memory provider for Hermes Agent

Gives [Hermes Agent](https://github.com/NousResearch/hermes-agent) persistent memory backed by MemMesh — the local-first open-source binary, or the hosted service.

Hermes' built-in memory is a pair of char-capped files (MEMORY.md at 2,200 chars, USER.md at 1,375) frozen into the system prompt at session start. That constraint is what a memory provider exists to remove.

## Install

```bash
pip install memmesh-hermes
hermes config set memory.provider memmesh
```

That's the whole install for local mode. The plugin manages the daemon.

> Hermes' bundled `plugins/memory/` directory is closed to new providers, so this ships as a pip entry point (`hermes_agent.memory_providers`). That's the supported path and it's not second-class: an entry point still gets the dashboard config panel and `hermes memmesh` subcommands, as long as it names a package.

## Modes

| Mode | What it talks to | Cost | Checkpoint |
|---|---|---|---|
| `local` *(default)* | a `memmesh serve-mcp` daemon this plugin starts | free, offline | ✗ |
| `local_external` | a MemMesh you already run | free | ✗ |
| `cloud` | app.memmesh.ai | paid | ✓ |

Config lives at `~/.hermes/memmesh/config.json`; every key can be overridden by env.

```jsonc
{
  "mode": "local",
  "bank_id_template": "hermes-{profile}",   // per-profile isolation
  "recall_limit": 10,
  "memory_mode": "hybrid"                   // hybrid | context | tools
}
```

Cloud mode also wants `MEMMESH_API_KEY` in `~/.hermes/.env` plus `project_id` (and optionally `brain_id`) in the config file.

## Two things worth knowing

**Local mode speaks MCP, not REST.** The open-source binary serves two different search paths: `POST /search` is a *filter* endpoint (`storage.query` — scope, ids, status, and a `text_match` LIKE, returned in storage order), while the hybrid semantic searcher is reachable only through the MCP tool surface. A plugin pointed at REST would work, return plausible-looking rows, and quietly deliver substring matching where you expected recall — nothing would error. So local mode talks to `memmesh serve-mcp` over JSON-RPC.

**Only cloud mode declares the compaction checkpoint.** `pre_compress_checkpoint_api_version = 2` is set per *instance*, not on the class. Version 2 is a promise that every successful `on_pre_compress()` means the transcript is durably committed, and an operator who sets `compression.checkpoint_required: true` is trusting that promise with data they can't get back. The OSS binary has no durable transcript archive, so in local mode the plugin doesn't make the claim.

## What it hooks

| Hook | Behaviour |
|---|---|
| `prefetch` | recall injected before each turn, gated by Hermes' own `is_trivial_prompt` |
| `sync_turn` | each completed turn queued to `observe` on a background thread |
| `on_pre_compress` | **cloud only** — durably archives the transcript before Hermes rewrites it, fail-closed |
| `on_delegation` | records subagent (task → result) as a reasoning trace |
| `on_memory_write` | mirrors Hermes' own MEMORY.md / USER.md writes into MemMesh |
| `recall_status` | deterministic `🧠 MemMesh — recalled N memories` indicator |
| `backup_paths` | declares the local DB so `hermes backup` doesn't miss it |
| tools | `memmesh_search`, `memmesh_observe` |

### On the trivial-prompt gate

Recall is skipped client-side for turns with no semantic signal — "ok", "thanks", "go ahead", slash commands. That saves the whole round-trip rather than just the server's work, which matters because prefetch sits in front of the model's first token.

It applies to **automatic** recall only. An explicit `memmesh_search` for "ok" still runs: it's a strange query, but somebody asked for it, and silently returning nothing would be a bug wearing an optimisation's clothes.

The predicate is Hermes' own `is_trivial_prompt`, imported rather than reimplemented. Two copies of that regex would drift, and the drift would be invisible — the failure mode is "recall silently stopped happening for some word nobody thought to test".

### On delegation traces

`verified` is deliberately **omitted**, never sent as `false`. The API treats `false` as "checked and found wrong" (kept as a counterexample) and omitted as "nothing checked it". A subagent returning a result is not evidence the result was correct, and sending `false` would mislabel every delegation as a known failure — removing it from procedure induction entirely, which is the opposite of the point.

### On the local daemon

Binds loopback only and is token-gated even there, because every other process on the machine can reach loopback. One daemon is shared per process, refcounted, and left running after the last release so `/reset` doesn't pay a cold start; `atexit` reclaims it. Logs land in `~/.hermes/logs/memmesh-daemon.log`.

## Failure behaviour

Everything fails **soft** except one path. Recall returns empty, writes are dropped from a full queue rather than blocking a turn, and a dead background writer can't take the session with it — a lost observation costs one memory, a blocked turn costs the conversation.

The exception is `on_pre_compress` under the v2 contract, which fails **closed**: it raises rather than returning, so compaction is blocked, the uncompressed transcript is preserved, and the attempt can be retried. Compaction is irreversible, and a provider whose job is to hold the evidence must not let it be destroyed on a best-effort basis.

Checkpoint writes are keyed by a SHA-256 of the transcript (salted with the bank id), because after a fail-closed block Hermes calls again with a transcript that's grown only slightly — successive attempts carry largely overlapping evidence, and a content digest is what makes a retry a no-op instead of a duplicate archive.

## Tests

```bash
pip install pytest && python -m pytest tests/ -q
```

Covers the logic that fails quietly: bank-id scoping (a dangling separator is a *different* bank, and the symptom is an agent that looks like it lost its memory), MCP result parsing (junk must degrade to empty recall, never an exception), and digest stability. Both `requests` and the Hermes runtime are stubbed, so no checkout or network is needed.

## License

Apache-2.0, same as MemMesh.
