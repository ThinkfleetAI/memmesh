# Remote MCP — wiring MemMesh into ChatGPT & Claude web chats

Local tools (Claude Code, Cursor, Codex, Windsurf) speak **stdio MCP** — `memmesh install`
wires those up automatically. Web chats are different: **ChatGPT** and **claude.ai**
custom connectors require a **remote MCP server over Streamable HTTP at a public HTTPS
URL** with auth. A local stdio server cannot be registered directly.

MemMesh ships this as `memmesh serve-mcp`.

## 1. Start the remote MCP server

```bash
# Pick a strong token (or let it auto-generate and print one).
export MEMMESH_MCP_TOKEN="$(openssl rand -hex 24)"
memmesh serve-mcp --http 127.0.0.1:7899
```

It prints the URL (`http://127.0.0.1:7899/mcp`) and the bearer token. Every request must
send `Authorization: Bearer <token>`; unauthenticated requests get `401`. `GET /health`
is unauthenticated for uptime checks.

## 2. Expose it over public HTTPS (tunnel)

The cloud services can't reach `localhost`, so put a tunnel in front.

```bash
# ngrok
ngrok http 7899
# → https://<random>.ngrok-free.app   (your MCP URL is that + /mcp)

# …or cloudflared
cloudflared tunnel --url http://127.0.0.1:7899
# → https://<random>.trycloudflare.com
```

For anything beyond a quick test, use a **named/persistent tunnel** so the URL is stable,
and run `serve-mcp` + the tunnel as long-lived services.

## 3. Register the connector

**Claude.ai** (Pro/Max/Team/Enterprise): Settings → Connectors → *Add custom connector* →
paste `https://<tunnel-host>/mcp`. Under request headers add
`Authorization: Bearer <token>`. Restart the chat; the `memory_*` tools appear.

**ChatGPT** (Plus/Pro/Business/Enterprise/Edu; write tools require Business/Enterprise/Edu):
enable **Settings → Connectors → Advanced → Developer mode**, then *Create* a connector
with URL `https://<tunnel-host>/mcp`, auth type *Custom headers* →
`Authorization: Bearer <token>`.

## 4. Verify

```bash
curl -s https://<tunnel-host>/mcp \
  -H "authorization: Bearer $MEMMESH_MCP_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | jq '.result.tools | length'
# → 11
```

## Security

- **The token is the only thing protecting your memory.** Anyone with the URL + token can
  read and write it. Use a long random token; rotate by restarting with a new one.
- Prefer a tunnel that lets you add IP allow-lists / access policies (Cloudflare Access,
  ngrok OAuth) in front of the endpoint.
- The remote endpoint is intentionally separate from `memmesh console` (loopback, no auth) —
  never expose the console.
- Full OAuth 2.1 (PKCE + Dynamic Client Registration) is the eventual "proper" auth; the
  bearer-token header is supported by both ChatGPT and Claude today and is the quickest path.

## How it fits

| Transport | Command | Used by |
|---|---|---|
| stdio MCP | `memmesh mcp` (via `memmesh install`) | Claude Code, Cursor, Codex, Windsurf |
| Streamable HTTP MCP | `memmesh serve-mcp` + tunnel | ChatGPT, Claude.ai web |
| Loopback REST + UI | `memmesh console` | the local management console (never expose) |

All three talk to the same `~/.memmesh/memory.db` and the same 11 memory tools.
