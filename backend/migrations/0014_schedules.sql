-- Recurring tasks. A schedule belongs to one person and runs as them, with the access they had when
-- they last used the app: the app refreshes that snapshot whenever they sign in, and a schedule
-- whose owner has not been seen for two weeks stops, so a person who leaves or loses access does
-- not keep a report running on their old permissions.

ALTER TABLE agent_tasks
  ADD COLUMN schedule_id         uuid,
  ADD COLUMN previous_task_id    uuid,
  -- A scheduled report waits for its owner's approval; a one-off task has no review.
  ADD COLUMN review_status       text CHECK (review_status IN ('pending', 'approved', 'rejected')),
  ADD COLUMN reviewed_at         timestamptz,
  ADD COLUMN edited_result       text,
  -- A fingerprint of the files the report used, so an unchanged week can be skipped.
  ADD COLUMN sources_fingerprint text;
CREATE INDEX agent_tasks_to_review ON agent_tasks (tenant_id, requested_by) WHERE review_status = 'pending';

CREATE TABLE agent_schedules (
  id                    uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id             uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  agent_id              uuid NOT NULL,
  owner                 text NOT NULL,
  owner_groups          text[] NOT NULL DEFAULT '{}',
  owner_email           text,
  identity_refreshed_at timestamptz NOT NULL DEFAULT now(),
  name                  text NOT NULL,
  instruction           text NOT NULL,
  cadence               jsonb NOT NULL,
  status                text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused')),
  pause_reason          text,
  monthly_budget_micros bigint,
  consecutive_failures  integer NOT NULL DEFAULT 0,
  last_run_at           timestamptz,
  last_task_id          uuid,
  last_note             text,
  created_at            timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  FOREIGN KEY (tenant_id, agent_id) REFERENCES agents (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX agent_schedules_by_owner ON agent_schedules (tenant_id, owner);

-- When each schedule is next due. Like the task queue it holds only IDs and times, so the worker
-- can find due work across firms without being able to read any firm's data.
CREATE TABLE agent_schedule_clock (
  schedule_id   uuid PRIMARY KEY,
  tenant_id     uuid NOT NULL,
  next_run_at   timestamptz NOT NULL,
  claimed_until timestamptz,
  FOREIGN KEY (tenant_id, schedule_id) REFERENCES agent_schedules (tenant_id, id) ON DELETE CASCADE
);

ALTER TABLE agent_schedules      ENABLE ROW LEVEL SECURITY; ALTER TABLE agent_schedules      FORCE ROW LEVEL SECURITY;
ALTER TABLE agent_schedule_clock ENABLE ROW LEVEL SECURITY; ALTER TABLE agent_schedule_clock FORCE ROW LEVEL SECURITY;
REVOKE ALL ON agent_schedules, agent_schedule_clock FROM PUBLIC;
GRANT SELECT, INSERT, UPDATE, DELETE ON agent_schedules, agent_schedule_clock TO app_worker;

CREATE POLICY tenant_isolation ON agent_schedules FOR ALL TO app_api, app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY own_firm ON agent_schedule_clock FOR ALL TO app_api
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY every_firm ON agent_schedule_clock FOR ALL TO app_worker USING (true) WITH CHECK (true);

-- The API creates, edits and removes a person's schedules and records their runs when asked to run
-- one now. It never writes a task's result; a person may only edit the report they are reviewing.
GRANT SELECT, INSERT, DELETE ON agent_schedules TO app_api;
GRANT UPDATE (name, instruction, cadence, status, pause_reason, monthly_budget_micros, consecutive_failures,
              owner_groups, owner_email, identity_refreshed_at, last_run_at, last_task_id, last_note)
  ON agent_schedules TO app_api;
GRANT SELECT, INSERT, UPDATE, DELETE ON agent_schedule_clock TO app_api;
GRANT UPDATE (review_status, reviewed_at, edited_result) ON agent_tasks TO app_api;
