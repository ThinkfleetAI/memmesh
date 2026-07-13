---
name: memmesh-cli
description: >
  MemMesh CLI + local MCP server — the zero-infra, no-API-key path to the same
  engine as the hosted SDK. Runs fully local over SQLite. Covers install (wires
  MCP config + the teaching skill into Claude Code / Cursor / Windsurf / Codex),
  and the memory subcommands (save / get / search / migrate / mcp / serve).
  TRIGGER when: user mentions "memmesh cli", "memmesh install", "thinkfleet-memory"
  binary, "npx @thinkfleet/memmesh", running `memmesh mcp`, or wants local
  memory with no hosted account.
  DO NOT TRIGGER for: the hosted TS SDK (use `memmesh-sdk`), or the always-on
  observe/recall behavior (use the `memmesh` skill).
license: Apache-2.0
metadata:
  author: thinkfleet
  category: ai-memory
  tags: "memory, cli, mcp, local, sqlite"
compatibility: The `memmesh` (a.k.a. `thinkfleet-memory`) Rust binary on PATH, or `npx @thinkfleet/memmesh` (dependency-free shim). Local mode needs no API key; data lives in ~/.thinkfleet-memory/memory.db.
---

# MemMesh CLI

The CLI drives the **same Rust engine** as the hosted platform, but fully local
over SQLite — no account, no key, no network. It is also the recommended way to
give **any** MCP-capable agent persistent memory.

## Install (one command, multi-tool)

```bash
npx @thinkfleet/memmesh install        # or: memmesh install
```

This auto-detects your installed AI tools and, for each, writes the MCP server
config **and** drops the teaching skill:

- Claude Code (`~/.claude.json` + `~/.claude/skills/`)
- Cursor (`~/.cursor/mcp.json`)
- Windsurf
- Codex CLI

Useful flags: `--dry-run` (preview), `--tool <name>` (one tool only),
`--skill-only` / `--mcp-only`, `--force` (overwrite existing config).

Verify wiring at any time:

```bash
memmesh doctor        # checks binary, MCP config, skill presence, hooks
```

## Memory subcommands

```bash
memmesh save --type preference --scope user --content "prefers pnpm over npm"
memmesh get <id>
memmesh search --project myapp --query "database"        # substring/scoped search
memmesh list  --project myapp --limit 20
memmesh migrate                                          # run pending DB migrations
```

## Run the MCP server

Most agents launch this for you via the config the installer wrote. To run it
by hand (stdio JSON-RPC 2.0):

```bash
memmesh mcp
```

The server exposes 15 tools — see the `memmesh` skill for the full list and the
observe/recall usage pattern. Highlights beyond basic CRUD:
`memory_predict`, `memory_build_context`, `memory_graph_reason`,
`memory_query_graph`, `memory_prefetch_related`, plus the client-LLM graph
extraction pair (`memory_extract_pending` / `memory_commit_extraction`) — the
engine never makes LLM calls; your agent's own model does the extraction.

## Local vs hosted

| | Local (CLI + MCP) | Hosted (SDK / Mesh Router) |
|---|---|---|
| Storage | SQLite (`~/.thinkfleet-memory/memory.db`) | Postgres, multi-tenant |
| API key | not required | `mm-…` key or Cognito JWT |
| Surface | 15 MCP tools + CLI | full TS SDK (`memmesh-sdk`) |
| Sync | CRDT-style bi-temporal push to server (optional) | authoritative |

Free tier caps local storage at 500 items (`free_tier.entry_cap` in
`~/.thinkfleet-memory/config.toml`).

## Config

`~/.thinkfleet-memory/config.toml` holds the sync URL, JWT token, and tier caps.
Point `sync.url` at your hosted mesh to push local memory up; leave it unset to
stay fully offline.
