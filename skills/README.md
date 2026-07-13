# MemMesh skills

Standalone, publishable skills for building with MemMesh in any
skills-compatible agent (Claude, Claude Code, Cursor, Codex, Windsurf, OpenCode).
Install individually:

```bash
npx skills add https://github.com/ThinkfleetAI/memmesh --skill memmesh
npx skills add https://github.com/ThinkfleetAI/memmesh --skill memmesh-sdk
npx skills add https://github.com/ThinkfleetAI/memmesh --skill memmesh-cli
npx skills add https://github.com/ThinkfleetAI/memmesh --skill memmesh-integrate
npx skills add https://github.com/ThinkfleetAI/memmesh --skill memmesh-test-integration
npx skills add https://github.com/ThinkfleetAI/memmesh --skill memmesh-migrate
```

## Reference skills (always-on / on demand)

| Skill | Use it for |
|---|---|
| [`memmesh`](./memmesh) | The always-on loop: observe every message, recall at session start. The engine decides what to save. |
| [`memmesh-sdk`](./memmesh-sdk) | Writing code against the hosted TS SDK — observe/search **plus** the predict / lattice / learning / verticals surface. |
| [`memmesh-cli`](./memmesh-cli) | The local CLI + MCP server (zero-infra, no API key) and one-command multi-tool install. |

## Pipeline skills (slash-command workflows)

| Skill | Use it for |
|---|---|
| [`memmesh-integrate`](./memmesh-integrate) | Wire MemMesh into an existing repo — TDD, additive, feature-flag-gated. |
| [`memmesh-test-integration`](./memmesh-test-integration) | Verify that integration end-to-end and produce a scorecard. |
| [`memmesh-migrate`](./memmesh-migrate) | Migrate onto MemMesh from Mem0 / Zep / a vector store, or Local → Hosted. |

## Operational + prediction skills

The everyday memory ops (`remember`, `forget`, `peek`, `tour`, `stats`, `dream`,
`pin`, …) and the prediction/graph skills (`predict`, `why`, `behaviors`,
`graph`, `benchmark`) ship in the Claude Code plugin at
[`integrations/memmesh-plugin/skills/`](../integrations/memmesh-plugin/skills).

## Ground truth for agents

- Docs index: https://docs.memmesh.ai/llms.txt
- Full docs: https://docs.memmesh.ai/llms-full.txt
