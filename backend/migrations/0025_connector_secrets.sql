-- Connector refresh tokens, kept encrypted in the database. A token lets us read a firm's files, so it
-- is never stored readable. Each row holds the token encrypted with a one-time data key, and that data
-- key encrypted by the firm's own KMS key (tenants.kms_key_arn). Opening a row needs KMS, so a copy of
-- the database alone holds nothing readable; see src/kms.rs.

CREATE TABLE connector_secrets (
  tenant_id   uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  -- Always "tenants/<firm>/..." (checked below), the same name the token store has always used.
  name        text NOT NULL,
  -- The KMS key that made the data key: the firm's own.
  key_id      text NOT NULL,
  -- The data key, encrypted by KMS under an encryption context naming this firm and this name.
  wrapped_key bytea NOT NULL,
  nonce       bytea NOT NULL,
  ciphertext  bytea NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, name),
  -- Even a bug cannot file one firm's secret under another firm's name.
  CHECK (starts_with(name, 'tenants/' || tenant_id::text || '/') AND position('..' in name) = 0),
  CHECK (octet_length(nonce) = 12)
);

ALTER TABLE connector_secrets ENABLE ROW LEVEL SECURITY; ALTER TABLE connector_secrets FORCE ROW LEVEL SECURITY;
REVOKE ALL ON connector_secrets FROM PUBLIC;
-- The API stores a token when someone connects a source; the worker reads and replaces it as it syncs.
GRANT SELECT, INSERT, UPDATE, DELETE ON connector_secrets TO app_api, app_worker;
CREATE POLICY tenant_isolation ON connector_secrets FOR ALL TO app_api, app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
