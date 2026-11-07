-- The Zoom meetings a person made from the app. Only these can be rescheduled or cancelled from the
-- app: it never lists, reads or changes any other meeting in their Zoom account. The host's start
-- link is never stored.

CREATE TABLE zoom_meetings (
  id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id    uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  owner        text NOT NULL,
  zoom_id      text NOT NULL,
  topic        text NOT NULL,
  -- In the person's own time zone: "2026-11-10T10:00:00".
  start_local  text NOT NULL,
  timezone     text NOT NULL,
  duration_minutes integer NOT NULL,
  join_url     text NOT NULL,
  status       text NOT NULL DEFAULT 'scheduled' CHECK (status IN ('scheduled', 'cancelled')),
  created_at   timestamptz NOT NULL DEFAULT now(),
  changed_at   timestamptz
);
CREATE INDEX zoom_meetings_by_owner ON zoom_meetings (tenant_id, owner, created_at DESC);

ALTER TABLE zoom_meetings ENABLE ROW LEVEL SECURITY; ALTER TABLE zoom_meetings FORCE ROW LEVEL SECURITY;
REVOKE ALL ON zoom_meetings FROM PUBLIC;
GRANT SELECT, INSERT, UPDATE ON zoom_meetings TO app_api;
CREATE POLICY tenant_isolation ON zoom_meetings FOR ALL TO app_api
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
