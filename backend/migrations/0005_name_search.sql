-- File names are searchable too: a question that names a file ("men's retreat") should reach
-- "2026 Men's Retreat MASTER" even when the wording inside the file differs.
ALTER TABLE documents
  ADD COLUMN name_tsv tsvector GENERATED ALWAYS AS (to_tsvector('english', name)) STORED;
CREATE INDEX documents_name_tsv ON documents USING gin (name_tsv);
