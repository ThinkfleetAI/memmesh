<div align="center">

# 🧠 MemMesh

### Persistent, local-first memory for AI agents — in a single Rust binary.

*Your agents forget everything between sessions. MemMesh gives them durable, typed, searchable memory that lives on your machine — no vector database, no search cluster, no mandatory LLM calls.*

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/ThinkfleetAI/memmesh?color=success)](https://github.com/ThinkfleetAI/memmesh/releases)
[![Rust](https://img.shields.io/badge/rust-1.78%2B-orange.svg)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-native-8A2BE2.svg)](https://modelcontextprotocol.io)
[![Stars](https://img.shields.io/github/stars/ThinkfleetAI/memmesh?style=social)](https://github.com/ThinkfleetAI/memmesh/stargazers)

[**memmesh.ai**](https://memmesh.ai) · [**Docs**](https://docs.memmesh.ai) · [**Releases**](https://github.com/ThinkfleetAI/memmesh/releases)

</div>

---

## Why MemMesh

Every agent framework bolts memory onto a hosted vector store and an LLM extraction call per message. That means a database to run, data leaving your machine, and a bill that scales with how much you remember.

MemMesh takes the opposite path: **one binary, one file, everything local.**

| | MemMesh | Typical agent-memory stack |
|---|---|---|
| **Runtime** | One Rust binary (SQLite or Postgres) | App + external vector DB (+ search service) |
| **Privacy** | Data never leaves your machine | Memories shipped to a hosted store |
| **LLM calls** | **None required** — heuristic capture + a zero-LLM knowledge graph | An extraction call per message |
| **Time model** | Bi-temporal (*when it happened* vs *when you learned it*) | Flat timestamps |
| **Protocol** | MCP-native — works in Claude Code, Cursor, Windsurf, Codex today | Framework-specific SDK |
| **License** | Apache-2.0, no limits | Varies |

## ⚡ 60-second quickstart

```sh
# 1. Build (Rust 1.78+)
cargo build --release --bin memmesh

# 2. Wire it into every AI tool on your machine — one command
./target/release/memmesh install

# 3. Watch it remember
./target/release/memmesh observe --content "Ryan prefers pnpm over npm for all projects."
./target/release/memmesh search  --query   "pnpm"
# → returns the stored memory, typed and timestamped
```

`memmesh install` detects each supported tool, merges an MCP server block into its config (your other MCP servers are untouched), and drops the teaching skill in the right place.

| Tool | MCP config | Skill location |
|---|---|---|
| Claude Code | `~/.claude.json` | `~/.claude/skills/memmesh/SKILL.md` |
| Cursor | `~/.cursor/mcp.json` | `~/.cursor/rules/memmesh/SKILL.md` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | *(MCP tool descriptions)* |
| Codex CLI | `~/.codex/config.toml` | *(MCP tool descriptions)* |

Restart the host tool afterward so it reloads its config. Useful flags: `--dry-run`, `--tool <id>` (repeatable), `--mcp-only` (skip the skill), `--no-hooks` (skip the Claude Code auto-observe hook), `--force`.

## 🛠️ CLI

The binary opens `~/.memmesh/memory.db` by default (override with `--db <path>`):

```sh
memmesh migrate                       # init / re-apply migrations (safe to repeat)
memmesh observe --content "We decided to use Postgres for the memory backend."
memmesh save --platform local --project alpha --type fact \
             --content "Sarah prefers email over phone"
memmesh get <id>                      # fetch by id
memmesh search --query "Sarah" --project alpha --limit 10
memmesh mcp                           # run as an MCP stdio server
```

## 🧰 MCP tools

Underscore names are canonical; dot names are accepted as legacy aliases.

**Available in the open-source engine (fully local):**

| Tool | What it does |
|---|---|
| `memory_observe` | Feed raw text; substantive statements are captured automatically (primary write path) |
| `memory_save` | Upsert a memory item with scope, type, content, importance |
| `memory_recall` | Fetch by id (reinforces the item on access) |
| `memory_search` | Filter by scope / project / agent / user / session + content match |
| `memory_list` | Most-recent items in a scope |
| `memory_delete` | Forget an item — soft delete (default, recoverable) or hard delete |
| `memory_supersede` | Record a correction (the old item is kept for provenance) |
| `memory_stats` | Total count of stored memories |
| `memory_extract_pending` / `memory_commit_extraction` | Client-LLM knowledge-graph extraction — **your** model, your key, your rate limit (the engine never calls an LLM) |

**Hosted mode ([memmesh.ai](https://memmesh.ai)) — the intelligence layer:**

| Tool | What it does |
|---|---|
| `memory_graph_reason` | Multi-hop reasoning over the knowledge graph |
| `memory_query_graph` | Point-in-time (bi-temporal) edge query |
| `memory_prefetch_related` | Anticipatory retrieval via spreading activation |
| `memory_build_context` | Full subject-context bundle (profile + patterns + predictions) |
| `memory_predict` | Forecast a subject's next events — calibrated, with provenance and honest abstention |

> The open-source engine is a complete, durable memory **store**. Hosted mode adds the intelligence layer — calibrated prediction, behavior discovery, and graph reasoning — on top of the same data. Skills for hosted-only tools degrade gracefully to `search` / `recall` on a local install.

## 🏗️ Architecture

A single Rust workspace; mode is chosen at runtime, not compile time — the same binary runs local (SQLite) or server (Postgres).

| Crate | Purpose |
|---|---|
| `core` | Domain types (`MemoryItem`, `MemoryScope`, `Contact`, …) + heuristic extraction |
| `storage` | `Storage` trait + `SqliteStore` + `PostgresStore` |
| `embed` / `embed-server` | Embedding generation + a standalone embedding service |
| `mcp` | MCP stdio protocol layer |
| `server` | Long-running services (MCP + HTTP) |
| `sync` | Bi-temporal sync between local and server stores |
| `audit` | Append-only audit log |
| `license` | Offline license-token verification (Ed25519) |
| `cli` | The `memmesh` binary |
| `eval` | Retrieval-quality evaluation harness |

## 🤝 Contributing

Issues and PRs welcome. MemMesh is Apache-2.0 and built to be embedded, extended, and self-hosted. If you're using it in a project, we'd love to hear about it.

## 📄 License

[Apache License 2.0](LICENSE). © 2026 ThinkFleet, Inc. and MemMesh contributors.
