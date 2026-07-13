# MemMesh

**Persistent, self-improving memory for AI agents.** A local-first memory
engine in Rust: one binary that runs as a desktop component (SQLite) or a
server backend (Postgres), speaks the [Model Context Protocol](https://modelcontextprotocol.io),
and wires into your AI tools with a single command.

[memmesh.ai](https://memmesh.ai) · [docs](https://docs.memmesh.ai) · Apache-2.0

---

## Why

LLM agents forget everything between sessions. MemMesh gives them a durable,
typed, scoped memory: facts, contacts, and relationships that persist, are
searchable, and improve as they're used — without shipping your data to a
third party. It runs on your machine, in your infrastructure, or both with
sync between them.

## Install

Build from source (Rust 1.78+):

```sh
cargo build --release --bin memmesh
# binary at ./target/release/memmesh
```

Wire it into every AI tool on your machine in one command:

```sh
memmesh install
```

That detects each supported tool, writes its MCP server config block, and
drops the agent teaching skill in the right place. Existing MCP servers in
your configs are preserved — configs are merged, never replaced.

| Tool | MCP config | Skill location |
|---|---|---|
| Claude Code | `~/.claude.json` | `~/.claude/skills/memmesh/SKILL.md` |
| Cursor | `~/.cursor/mcp.json` | `~/.cursor/rules/memmesh/SKILL.md` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | (MCP tool descriptions) |
| Codex CLI | `~/.codex/config.toml` | (MCP tool descriptions) |

Restart the host tool afterward so it picks up the new config. Useful flags:
`--dry-run`, `--tool <id>` (repeatable), `--force`, `--skill-only`,
`--mcp-only`.

## Usage

The binary opens `~/.memmesh/memory.db` by default (override with `--db <path>`):

```sh
# Init / re-apply migrations (safe to repeat)
memmesh migrate

# Save a memory item
memmesh save \
    --platform plat_test --project proj_alpha \
    --type fact --scope project \
    --content "Sarah prefers email over phone"

# Fetch by id
memmesh get mem_demo_1

# Search (scope/project-filtered)
memmesh search --query "Sarah" --project proj_alpha --limit 10

# Run as an MCP stdio server
memmesh mcp
```

### Tools exposed over MCP

Underscore names are canonical; dot names are accepted as legacy aliases.

| Tool | What it does |
|---|---|
| `memory_observe` | Feed raw text; the engine decides what to save (primary write path) |
| `memory_save` | Upsert a memory item with scope, type, content, importance (rare) |
| `memory_recall` | Fetch by id (reinforces the item on access) |
| `memory_search` | Filter by scope/project/agent/user/session + content match |
| `memory_list` | Most-recent items in a scope |
| `memory_delete` | Forget an item — soft-reject (default, sync-safe) or hard delete |
| `memory_supersede` | Record a correction (old item kept for provenance) |
| `memory_stats` | Counts by type/scope/status + age span |
| `memory_extract_pending` / `memory_commit_extraction` | Client-LLM knowledge-graph extraction |
| `memory_graph_reason` | Multi-hop reasoning over the knowledge graph |
| `memory_query_graph` | Point-in-time (bi-temporal) edge query |
| `memory_prefetch_related` | Anticipatory retrieval via spreading activation |
| `memory_build_context` | Full subject context bundle (profile + patterns + predictions) |
| `memory_predict` | Forecast a subject's next events, calibrated + with provenance |

## Architecture

A single Rust workspace:

| Crate | Purpose |
|---|---|
| `core` | Domain types (`MemoryItem`, `MemoryScope`, `Contact`, …) + algorithms |
| `storage` | `Storage` trait + `SqliteStore` + `PostgresStore` |
| `embed` / `embed-server` | Embedding generation + a standalone embedding service |
| `mcp` | MCP stdio protocol layer |
| `server` | Long-running services (MCP + HTTP) |
| `sync` | Bi-temporal sync between local and server stores |
| `audit` | Append-only audit log |
| `license` | Offline license-token verification (Ed25519) |
| `cli` | The `memmesh` binary |
| `eval` | Retrieval-quality evaluation harness |

Mode is chosen at runtime, not compile time — the same binary runs local
(SQLite) or server (Postgres).

## License

[Apache License 2.0](LICENSE). © 2026 ThinkFleet, Inc. and MemMesh
contributors.
