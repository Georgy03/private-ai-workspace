-- What each person's AI answers cost, and the monthly limit per person. Counts and costs only:
-- never a question, an answer or document text.
CREATE TABLE usage_events (
  id            bigserial PRIMARY KEY,
  tenant_id     uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  user_id       text NOT NULL,
  at            timestamptz NOT NULL DEFAULT now(),
  model         text NOT NULL,
  input_tokens  bigint NOT NULL CHECK (input_tokens >= 0),
  output_tokens bigint NOT NULL CHECK (output_tokens >= 0),
  cost_micros   bigint NOT NULL CHECK (cost_micros >= 0)   -- millionths of a dollar
);
CREATE INDEX usage_events_by_person ON usage_events (tenant_id, user_id, at);

CREATE TABLE firm_settings (
  tenant_id            uuid PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
  -- Most one person may spend on AI answers in a calendar month. Null means the server default.
  monthly_limit_micros bigint CHECK (monthly_limit_micros >= 0)
);

ALTER TABLE usage_events  ENABLE ROW LEVEL SECURITY; ALTER TABLE usage_events  FORCE ROW LEVEL SECURITY;
ALTER TABLE firm_settings ENABLE ROW LEVEL SECURITY; ALTER TABLE firm_settings FORCE ROW LEVEL SECURITY;

CREATE POLICY own_firm_read   ON usage_events FOR SELECT TO app_api USING (tenant_id = app_tenant());
CREATE POLICY own_firm_append ON usage_events FOR INSERT TO app_api WITH CHECK (tenant_id = app_tenant());
CREATE POLICY own_firm ON firm_settings FOR ALL TO app_api
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());

REVOKE ALL ON usage_events, firm_settings FROM PUBLIC;
-- The ledger is append-only: no UPDATE or DELETE for any application role.
GRANT SELECT, INSERT ON usage_events TO app_api;
GRANT USAGE ON SEQUENCE usage_events_id_seq TO app_api;
GRANT SELECT, INSERT, UPDATE ON firm_settings TO app_api;
