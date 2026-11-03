-- Meaning vectors for passages, so a question finds a passage that says the same thing in other
-- words. Filled in by the worker after a passage is stored; null until then. The vector is plain
-- numbers (not the pgvector type) so this works on any Postgres; where pgvector is available, it
-- can replace the comparison below without changing what is stored.
ALTER TABLE chunks
  ADD COLUMN embedding real[],
  ADD COLUMN embedding_model text;

-- Finds the passages still waiting for a vector without reading the whole table.
CREATE INDEX chunks_unembedded ON chunks (tenant_id) WHERE embedding IS NULL;

-- Dot product of two vectors. For vectors of length one this is their cosine similarity.
CREATE FUNCTION embedding_dot(a real[], b real[]) RETURNS real
  LANGUAGE sql IMMUTABLE PARALLEL SAFE
  AS $$ SELECT COALESCE(sum(x * y), 0)::real FROM unnest(a, b) AS t(x, y) $$;
