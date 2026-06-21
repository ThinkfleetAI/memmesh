# Manual Setup — ThinkFleet Memory

Two ways to install:

1. **ThinkFleet Desktop** (recommended) — one click, auto-wires every AI tool on your machine, manages auth and cloud sync. [Download →](https://memmesh.ai/download)
2. **Manual setup** (this page) — for devs who want to BYO install. Everything below is exactly what the desktop does, just by hand.

Once installed, the same memory persists across every AI tool you use — Claude Code, Cursor, Codex, Cline, Continue, Goose, Zed, Windsurf — and follows you across machines if you enable cloud sync.

---

## 1. Install the binary

### macOS / Linux

```sh
curl -fsSL https://memmesh.ai/install.sh | sh
```

Or build from source:

```sh
git clone https://github.com/ThinkfleetAI/memmesh
cd memmesh
cargo build --release
sudo cp target/release/memmesh /usr/local/bin/
```

Verify:

```sh
memmesh --version
```

The binary stores memories in `~/.memmesh/memory.db` (SQLite, single-file, ~20MB for typical use).

---

## 2. One-command wire-up

```sh
memmesh install
```

This auto-detects every supported AI tool on your machine and wires in:

- An MCP server entry pointing at the binary
- A teaching skill / instructions file so the model knows when to use memory
- For Claude Code: a `SessionStart` hook (loads memories on start) and `UserPromptSubmit` hook (auto-observes every prompt)
- For Codex: a directive block in `~/.codex/AGENTS.md`

Re-run any time — it's idempotent. Other vendors' MCP servers stay untouched.

Verify with the doctor:

```sh
memmesh doctor
```

Sample output:

```
✓ Claude Code [claude-code]
    ✓ MCP config: /Users/you/.claude.json
    ✓ skill: /Users/you/.claude/skills/memmesh/SKILL.md
    ✓ hook: /Users/you/.claude/settings.json
✓ Codex CLI [codex]
    ✓ MCP config: /Users/you/.codex/config.toml
    ✓ skill: /Users/you/.codex/instructions/memmesh/SKILL.md
    ✓ instructions: /Users/you/.codex/AGENTS.md
```

Restart any running AI tools so they pick up the new config.

---

## 3. Per-tool reference (what `install` actually writes)

If you'd rather wire it up by hand, here's exactly what goes where.

### Claude Code

**MCP** — merged into `~/.claude.json`:

```json
{
  "mcpServers": {
    "memmesh": {
      "command": "/usr/local/bin/memmesh",
      "args": ["--db", "/Users/you/.memmesh/memory.db", "mcp"]
    }
  }
}
```

**Hooks** — merged into `~/.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [{
      "matcher": "*",
      "hooks": [{
        "type": "command",
        "command": "/usr/local/bin/memmesh search --project \"$(basename \"$PWD\")\" --limit 30 --format claude-context 2>/dev/null || true"
      }]
    }],
    "UserPromptSubmit": [{
      "matcher": "*",
      "hooks": [{
        "type": "command",
        "command": "/usr/local/bin/memmesh observe --role user --json 2>/dev/null || true"
      }]
    }]
  }
}
```

**Skill** — dropped at `~/.claude/skills/memmesh/SKILL.md`. Run `memmesh skill print` to get the content.

### Codex CLI

**MCP** — merged into `~/.codex/config.toml`:

```toml
[mcp_servers.memmesh]
command = "/usr/local/bin/memmesh"
args = ["--db", "/Users/you/.memmesh/memory.db", "mcp"]
```

**Instructions** — appended to `~/.codex/AGENTS.md` (Codex doesn't auto-load skill files; AGENTS.md is the only surface that reaches the model). Block is wrapped in `<!-- BEGIN memmesh -->` / `<!-- END memmesh -->` markers and tells the model to call `memory_search` at session start and `memory_observe` after every user message.

### Cursor

**MCP** — `~/.cursor/mcp.json` (same JSON shape as Claude Code).
**Skill** — `~/.cursor/rules/memmesh/SKILL.md`.

### Windsurf

**MCP** — `~/.codeium/windsurf/mcp_config.json` (same JSON shape).
**Skill** — `~/.codeium/windsurf/memories/memmesh/SKILL.md`.

### Cline

**MCP** — `~/.cline/mcp.json` (same JSON shape).
**Skill** — `~/Documents/Cline/Rules/memmesh/SKILL.md`.

### Continue

**MCP** — per-server YAML at `~/.continue/mcpServers/memmesh.yaml`:

```yaml
name: ThinkFleet Memory
version: 0.1.0
schema: v1
mcpServers:
  - name: memmesh
    command: /usr/local/bin/memmesh
    args: ["--db", "/Users/you/.memmesh/memory.db", "mcp"]
```

**Skill** — `~/.continue/rules/memmesh/SKILL.md`.

### Goose (Block)

**MCP** — merged into `~/.config/goose/config.yaml` under `extensions:`:

```yaml
extensions:
  memmesh:
    enabled: true
    name: memmesh
    type: stdio
    cmd: /usr/local/bin/memmesh
    args: ["--db", "/Users/you/.memmesh/memory.db", "mcp"]
    timeout: 300
```

### Zed

**MCP** — `~/.config/zed/settings.json` under the `context_servers` key (Zed's naming, not `mcpServers`):

```json
{
  "context_servers": {
    "memmesh": {
      "command": "/usr/local/bin/memmesh",
      "args": ["--db", "/Users/you/.memmesh/memory.db", "mcp"]
    }
  }
}
```

Zed has no global rules file. The MCP tool descriptions themselves (visible to every Zed session) carry the usage guidance.

---

## 4. Bring your own agent (HTTP API)

Any tool that can make an HTTP call can use memory. Run a local HTTP server:

```sh
memmesh serve --http 127.0.0.1:7878
```

Loopback only — no auth required for local-process access.

**Observe (auto-extract from raw text):**

```sh
curl -X POST http://127.0.0.1:7878/observe \
  -H 'Content-Type: application/json' \
  -d '{"text": "remember: I prefer pnpm over npm", "role": "user"}'
```

**Search:**

```sh
curl 'http://127.0.0.1:7878/search?projectId=growth-os&limit=20'
```

**Save explicitly:**

```sh
curl -X POST http://127.0.0.1:7878/memory \
  -H 'Content-Type: application/json' \
  -d '{"id": "...", "platformId": "local", "type": "preference", "content": "...", "scope": "user"}'
```

Full OpenAPI spec at [/openapi.json](http://127.0.0.1:7878/openapi.json) once the server is running.

For agents that speak MCP but aren't in the install list above, just point them at:

```
command: memmesh
args:    ["mcp"]
```

The MCP server exposes 5 tools: `memory_observe`, `memory_search`, `memory_recall`, `memory_save`, `memory_list`.

---

## 5. Cloud sync (optional)

Free tier holds 500 memories locally. To sync across devices and unlock unlimited storage:

1. Sign in at [memmesh.ai](https://memmesh.ai) — get your platform id and API token.
2. Wire your local engine to your account:

```sh
memmesh config set-sync \
  --url https://app.memmesh.ai \
  --token <your-api-token> \
  --platform-id <your-platform-id>
```

3. Run the sync daemon (lives inside `serve`):

```sh
memmesh serve --http 127.0.0.1:7878
```

Or push once and exit:

```sh
memmesh sync
```

To stop syncing:

```sh
memmesh config clear-sync
```

Pricing: see [memmesh.ai/pricing](https://memmesh.ai/pricing).

---

## 6. Troubleshooting

**"Agent has the tools but isn't using memory."** 99% of the time this is a missing hook or instructions file. Run `memmesh doctor` — it'll tell you exactly what's wrong per tool.

**"Memories saved via one agent aren't visible to another."** All agents share `~/.memmesh/memory.db`. If they differ, an agent's MCP config is pointing at a different `--db` path. Check with `memmesh doctor` (the `MCP config` line shows the resolved binary; run that binary's `--db` to see the path).

**"Restored my dotfiles and now nothing works."** Just re-run `memmesh install`. Idempotent — won't disturb other tools' configs.

**"Claude Code didn't pick up the changes."** Restart Claude Code. Hooks are loaded at startup; settings.json changes don't hot-reload.

**Reverting:** `memmesh install` is idempotent and merges — it never removes other vendors' entries. To remove ThinkFleet from a single tool's config, delete the `memmesh` entry from that tool's MCP config file by hand. To remove from all tools, manually delete:

- `~/.claude.json` → `mcpServers.memmesh`
- `~/.claude/settings.json` → `hooks.SessionStart` and `hooks.UserPromptSubmit` entries whose command contains `memmesh`
- `~/.codex/config.toml` → `[mcp_servers.memmesh]`
- `~/.codex/AGENTS.md` → block between `<!-- BEGIN memmesh -->` and `<!-- END memmesh -->`
- (and equivalent for Cursor, Windsurf, Cline, Continue, Goose, Zed)

---

## What you just enabled

Every AI tool on your machine now has access to one shared, persistent memory that:

- Remembers your preferences, project decisions, and recurring facts across sessions
- Surfaces relevant context at the start of every conversation (Claude Code via hook; others via skill instructions)
- Auto-extracts facts from your prompts (no judgment required — the engine decides what's worth saving)
- Optionally syncs across your devices via cloud (paid tier)

The point is **you never have to repeat yourself**, in any AI tool, ever again.
