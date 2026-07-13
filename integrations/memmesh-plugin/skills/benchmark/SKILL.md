---
name: benchmark
description: >
  Run MemMesh's competitive benchmark harness (LOCOMO / BEAM) to compare
  retrieval quality, tokens, latency, and cost against Mem0, Zep, full-context,
  and naive-RAG baselines. Use when the user wants proof MemMesh is better, is
  evaluating a migration, or asks "how does this compare to mem0".
---

# benchmark

Put numbers on the comparison. MemMesh ships a real benchmark harness that runs
the public LOCOMO dataset end-to-end against competing systems.

## What it compares

Systems: `thinkfleet` (MemMesh) vs `full_context` vs `naive_rag`, and — with keys
— Mem0 / Zep. Metrics: answer accuracy (rubric-scored), tokens consumed, latency,
and cost per conversation.

## Run it

The harness lives in the engine repo at `crates/eval/competitive/`:
```bash
cd crates/eval/competitive
python bench.py --systems thinkfleet,mem0,full_context --dataset locomo
# results land in results/
```
(Set the competitors' API keys via env for a head-to-head; without them you still
get MemMesh vs full-context vs naive-RAG.)

## Report honestly

MemMesh's positioning is **calibration over raw accuracy** — "80% means 80%" and
honest abstention beat a slightly higher accuracy with overconfident wrong
answers. So report the full picture:

- accuracy **and** calibration error,
- tokens / latency / cost (MemMesh's retrieval is far cheaper than full-context),
- where MemMesh abstained vs. where a competitor answered confidently and wrong.

Don't cherry-pick a single accuracy number. If a competitor wins on one axis, say
so, and show where MemMesh's calibration/cost advantage pays off.

## Cost gating

Prove the win on the cheap tiers (LOCOMO, BEAM-100K) before spending on
BEAM-1M/10M — a single 10M-token conversation is expensive. Escalate tiers only
once the cheaper tier shows a clear, defensible lead.
