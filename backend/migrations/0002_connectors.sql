-- Connector state: where each sync stands, what a document's text status is, and a work queue
-- the sync workers claim from.

ALTER TABLE connections
  ADD COLUMN config                  jsonb NOT NULL DEFAULT '{}',  -- e.g. {"drive_id": "...", "domains": ["firm.com"]}
  ADD COLUMN cursor                  text,                         -- provider change cursor
  ADD COLUMN last_sync_at            timestamptz,
  ADD COLUMN last_error              text,                         -- short, never contains tokens
  ADD COLUMN permissions_refreshed_at timestamptz;

ALTER TABLE connections ADD CONSTRAINT connections_status_check
  CHECK (status IN ('active', 'needs_reauth', 'disabled'));

ALTER TABLE documents
  ADD COLUMN status    text NOT NULL DEFAULT 'indexed' CHECK (status IN ('indexed', 'metadata_only')),
  ADD COLUMN synced_at timestamptz NOT NULL DEFAULT now();

-- The API records a new connection and removes one on disconnect. Deleting a connection removes
-- its documents and chunks through the cascading keys.
GRANT SELECT (last_sync_at, last_error) ON connections TO app_api;
GRANT INSERT, DELETE ON connections TO app_api;

-- One row per connection that should keep syncing. It holds only IDs and times, so it is not a
-- place where firm data can leak. Workers see every row because they must find work across
-- firms; the API sees only its own firm's row.
CREATE TABLE sync_queue (
  connection_id uuid PRIMARY KEY,
  tenant_id     uuid NOT NULL,
  run_after     timestamptz NOT NULL DEFAULT now(),
  claimed_until timestamptz,
  FOREIGN KEY (tenant_id, connection_id) REFERENCES connections (tenant_id, id) ON DELETE CASCADE
);

ALTER TABLE sync_queue ENABLE ROW LEVEL SECURITY;
ALTER TABLE sync_queue FORCE ROW LEVEL SECURITY;
CREATE POLICY own_firm ON sync_queue FOR ALL TO app_api
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY every_firm ON sync_queue FOR ALL TO app_worker
  USING (true) WITH CHECK (true);
REVOKE ALL ON sync_queue FROM PUBLIC;
GRANT SELECT, INSERT ON sync_queue TO app_api;
GRANT UPDATE (run_after) ON sync_queue TO app_api;
GRANT SELECT, INSERT, UPDATE, DELETE ON sync_queue TO app_worker;
