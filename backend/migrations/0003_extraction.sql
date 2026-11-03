-- Which version of the file reader produced a document's text. When the reader improves, files
-- read by an older version are read again.
ALTER TABLE documents ADD COLUMN parser_version integer NOT NULL DEFAULT 0;
