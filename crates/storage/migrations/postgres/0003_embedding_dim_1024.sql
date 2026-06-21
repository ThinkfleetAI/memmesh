-- ────────────────────────────────────────────────────────────
-- 0003 — embedding dimension 1536 → 1024
-- ────────────────────────────────────────────────────────────
-- The chosen local embedding model is bge-large-en-v1.5, which produces
-- 1024-dim vectors. The original schema provisioned `embedding vector(1536)`
-- (OpenAI-style), but nothing has ever written to it, so this is a
-- zero-data-loss change: drop the empty column + its HNSW index and recreate
-- both at 1024. pgvector can't ALTER a vector column's dimension in place, so
-- drop/recreate is the supported path.
--
-- sqlite needs no equivalent migration: there the embedding is a dimension-
-- agnostic BLOB.

DROP INDEX IF EXISTS idx_memory_item_embedding_hnsw;

ALTER TABLE clawdbot_memory_item DROP COLUMN IF EXISTS embedding;
ALTER TABLE clawdbot_memory_item ADD COLUMN embedding vector(1024);

CREATE INDEX IF NOT EXISTS idx_memory_item_embedding_hnsw
    ON clawdbot_memory_item USING hnsw (embedding vector_cosine_ops);
