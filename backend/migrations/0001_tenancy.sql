-- Per-firm isolation in a shared database.
--
-- Every table that holds firm data carries tenant_id. Row-level security (forced, so it applies
-- to the table owner too) restricts every row to the tenant the current transaction was opened
-- for. The tenant is set only by the server from a verified token (see src/db.rs). A session with
-- no tenant set sees nothing.
--
-- The application roles below have no LOGIN, no superuser and no BYPASSRLS. Deployment creates
-- the login users as members of them.

DO $$
BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'app_api') THEN
    CREATE ROLE app_api NOLOGIN NOSUPERUSER NOBYPASSRLS;
  END IF;
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'app_worker') THEN
    CREATE ROLE app_worker NOLOGIN NOSUPERUSER NOBYPASSRLS;
  END IF;
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'app_provisioner') THEN
    CREATE ROLE app_provisioner NOLOGIN NOSUPERUSER NOBYPASSRLS;
  END IF;
END
$$;

CREATE FUNCTION app_tenant() RETURNS uuid
LANGUAGE sql STABLE
AS $$ SELECT nullif(current_setting('app.tenant_id', true), '')::uuid $$;

CREATE TABLE tenants (
  id          uuid PRIMARY KEY,
  name        text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
  kms_key_arn text,
  status      text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended')),
  created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE connections (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id  uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  kind       text NOT NULL CHECK (kind IN ('microsoft365', 'google_drive')),
  -- Name of the secret that holds the refresh token. The token itself is never stored here.
  secret_ref text NOT NULL,
  status     text NOT NULL DEFAULT 'active',
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id)
);

CREATE TABLE documents (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id     uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  connection_id uuid NOT NULL,
  external_id   text NOT NULL,
  name          text NOT NULL,
  path          text NOT NULL DEFAULT '',
  mime_type     text NOT NULL DEFAULT '',
  modified_at   bigint NOT NULL DEFAULT 0,
  content_hash  text NOT NULL DEFAULT '',
  -- Principals allowed to read this file in the source system, e.g. 'user:<sub>', 'group:<id>'.
  -- Empty means unknown or unreadable, and the file is hidden from everyone.
  readable_by   text[] NOT NULL DEFAULT '{}',
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, connection_id, external_id),
  -- Composite keys stop a row from pointing at another tenant's parent.
  FOREIGN KEY (tenant_id, connection_id) REFERENCES connections (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE chunks (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id   uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  document_id uuid NOT NULL,
  ordinal     integer NOT NULL,
  text        text NOT NULL,
  location    jsonb NOT NULL DEFAULT '{}',
  tsv         tsvector GENERATED ALWAYS AS (to_tsvector('english', text)) STORED,
  FOREIGN KEY (tenant_id, document_id) REFERENCES documents (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX chunks_tsv ON chunks USING gin (tsv);

-- Metadata only: never document text, questions or tokens.
CREATE TABLE audit_events (
  id        bigserial PRIMARY KEY,
  tenant_id uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  at        timestamptz NOT NULL DEFAULT now(),
  actor     text NOT NULL,
  action    text NOT NULL,
  detail    jsonb NOT NULL DEFAULT '{}'
);

ALTER TABLE tenants      ENABLE ROW LEVEL SECURITY; ALTER TABLE tenants      FORCE ROW LEVEL SECURITY;
ALTER TABLE connections  ENABLE ROW LEVEL SECURITY; ALTER TABLE connections  FORCE ROW LEVEL SECURITY;
ALTER TABLE documents    ENABLE ROW LEVEL SECURITY; ALTER TABLE documents    FORCE ROW LEVEL SECURITY;
ALTER TABLE chunks       ENABLE ROW LEVEL SECURITY; ALTER TABLE chunks       FORCE ROW LEVEL SECURITY;
ALTER TABLE audit_events ENABLE ROW LEVEL SECURITY; ALTER TABLE audit_events FORCE ROW LEVEL SECURITY;

CREATE POLICY tenant_self ON tenants FOR SELECT TO app_api, app_worker
  USING (id = app_tenant());
-- The provisioner creates and removes firms. It has no access to firm data tables.
CREATE POLICY tenant_provisioning ON tenants FOR ALL TO app_provisioner
  USING (true) WITH CHECK (true);

CREATE POLICY tenant_isolation ON connections FOR ALL TO app_api, app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY tenant_isolation ON documents FOR ALL TO app_api, app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY tenant_isolation ON chunks FOR ALL TO app_api, app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY tenant_read ON audit_events FOR SELECT TO app_api, app_worker
  USING (tenant_id = app_tenant());
CREATE POLICY tenant_append ON audit_events FOR INSERT TO app_api, app_worker
  WITH CHECK (tenant_id = app_tenant());

REVOKE ALL ON tenants, connections, documents, chunks, audit_events FROM PUBLIC;

GRANT SELECT ON tenants TO app_api, app_worker;
GRANT SELECT, INSERT, UPDATE, DELETE ON tenants TO app_provisioner;

-- The API can read search data but never write it, and never sees connector secret names.
GRANT SELECT (id, tenant_id, kind, status) ON connections TO app_api;
GRANT SELECT ON documents, chunks TO app_api;
GRANT SELECT, INSERT, UPDATE, DELETE ON connections, documents, chunks TO app_worker;

-- Append-only: no UPDATE or DELETE for any application role.
GRANT SELECT, INSERT ON audit_events TO app_api, app_worker;
GRANT USAGE ON SEQUENCE audit_events_id_seq TO app_api, app_worker;
