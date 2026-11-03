-- What the sync dashboard shows: whether a sync is running and how far it has got, and why a
-- file is stored by name only (so a person can tell a scan from a file that could not be read).
ALTER TABLE documents ADD COLUMN note text;   -- null for readable files

ALTER TABLE connections
  ADD COLUMN sync_started_at   timestamptz,    -- set while a sync runs, null when idle
  ADD COLUMN sync_files_read   integer NOT NULL DEFAULT 0,
  ADD COLUMN sync_files_failed integer NOT NULL DEFAULT 0;

GRANT SELECT (sync_started_at, sync_files_read, sync_files_failed, permissions_refreshed_at)
  ON connections TO app_api;
