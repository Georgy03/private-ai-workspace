-- Shared AI workers. An agent is a saved, versioned configuration; a task is one run of it with
-- its own conversation. Tasks run with the permissions of the person who started them, so an agent
-- can never read what that person could not open.

CREATE TABLE agents (
  id                    uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id             uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  current_version       integer NOT NULL DEFAULT 1,
  status                text NOT NULL DEFAULT 'available' CHECK (status IN ('available', 'paused', 'archived')),
  -- Who may give it work: the word 'everyone', or people and groups (user:..., group:..., email:..., domain:...).
  audience              text[] NOT NULL DEFAULT '{everyone}',
  monthly_budget_micros bigint CHECK (monthly_budget_micros >= 0),      -- null means no limit
  max_concurrent        integer NOT NULL DEFAULT 3 CHECK (max_concurrent BETWEEN 1 AND 20),
  created_by            text NOT NULL,
  created_at            timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id)
);

-- Every change to what an agent is or how it behaves is a new, permanent version.
CREATE TABLE agent_versions (
  tenant_id    uuid NOT NULL,
  agent_id     uuid NOT NULL,
  version      integer NOT NULL,
  name         text NOT NULL,
  role         text NOT NULL DEFAULT '',
  description  text NOT NULL DEFAULT '',
  icon         text NOT NULL DEFAULT 'sparkles',
  color        text NOT NULL DEFAULT 'graphite',
  instructions text NOT NULL,
  tools        text[] NOT NULL DEFAULT '{}',
  model        text NOT NULL DEFAULT 'standard' CHECK (model IN ('standard', 'advanced')),
  effort       text NOT NULL DEFAULT 'medium' CHECK (effort IN ('low', 'medium', 'high')),
  note         text NOT NULL DEFAULT '',
  created_by   text NOT NULL,
  created_at   timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, agent_id, version),
  FOREIGN KEY (tenant_id, agent_id) REFERENCES agents (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE agent_tasks (
  id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id        uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  agent_id         uuid NOT NULL,
  agent_version    integer NOT NULL,
  requested_by     text NOT NULL,
  -- The requester's identity when the task was started: what the agent may read is what they may read.
  requester_groups text[] NOT NULL DEFAULT '{}',
  requester_email  text,
  title            text NOT NULL,
  instruction      text NOT NULL,
  status           text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'done', 'failed', 'cancelled')),
  result           text,
  error            text,
  sources          jsonb NOT NULL DEFAULT '[]',
  turns            integer NOT NULL DEFAULT 0,
  input_tokens     bigint NOT NULL DEFAULT 0,
  output_tokens    bigint NOT NULL DEFAULT 0,
  cost_micros      bigint NOT NULL DEFAULT 0,
  created_at       timestamptz NOT NULL DEFAULT now(),
  started_at       timestamptz,
  finished_at      timestamptz,
  UNIQUE (tenant_id, id),
  FOREIGN KEY (tenant_id, agent_id) REFERENCES agents (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX agent_tasks_by_person ON agent_tasks (tenant_id, requested_by, created_at DESC);
CREATE INDEX agent_tasks_by_agent ON agent_tasks (tenant_id, agent_id, created_at DESC);

-- What happened, in words, without document text: the activity history.
CREATE TABLE agent_task_events (
  id        bigserial PRIMARY KEY,
  tenant_id uuid NOT NULL,
  task_id   uuid NOT NULL,
  at        timestamptz NOT NULL DEFAULT now(),
  kind      text NOT NULL CHECK (kind IN ('created', 'started', 'tool', 'note', 'finished', 'failed', 'cancelled')),
  summary   text NOT NULL,
  FOREIGN KEY (tenant_id, task_id) REFERENCES agent_tasks (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX agent_task_events_by_task ON agent_task_events (tenant_id, task_id, id);

-- One task's whole conversation with the model, kept so it can carry on after a restart. Only the
-- worker reads it, and nothing ever reads another task's.
CREATE TABLE agent_task_messages (
  tenant_id uuid NOT NULL,
  task_id   uuid NOT NULL,
  seq       integer NOT NULL,
  role      text NOT NULL CHECK (role IN ('user', 'assistant')),
  content   jsonb NOT NULL,
  PRIMARY KEY (tenant_id, task_id, seq),
  FOREIGN KEY (tenant_id, task_id) REFERENCES agent_tasks (tenant_id, id) ON DELETE CASCADE
);

-- Work waiting to be done. Like the sync queue, it holds only IDs and times, so the worker can find
-- work across firms without being able to read any firm's data.
CREATE TABLE agent_queue (
  task_id       uuid PRIMARY KEY,
  tenant_id     uuid NOT NULL,
  run_after     timestamptz NOT NULL DEFAULT now(),
  claimed_until timestamptz,
  FOREIGN KEY (tenant_id, task_id) REFERENCES agent_tasks (tenant_id, id) ON DELETE CASCADE
);

DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['agents', 'agent_versions', 'agent_tasks', 'agent_task_events', 'agent_task_messages', 'agent_queue']
  LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON %I FROM PUBLIC', t);
    EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON %I TO app_worker', t);
  END LOOP;
END $$;
GRANT USAGE ON SEQUENCE agent_task_events_id_seq TO app_api, app_worker;

CREATE POLICY tenant_isolation ON agents FOR ALL TO app_api, app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY tenant_isolation ON agent_versions FOR ALL TO app_api, app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY tenant_isolation ON agent_tasks FOR ALL TO app_api, app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY tenant_isolation ON agent_task_events FOR ALL TO app_api, app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY tenant_isolation ON agent_task_messages FOR ALL TO app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY own_firm ON agent_queue FOR ALL TO app_api
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY every_firm ON agent_queue FOR ALL TO app_worker USING (true) WITH CHECK (true);

-- The API creates and configures agents and starts and cancels tasks. It never touches a task's
-- conversation, and it cannot write a task's result: only the worker does.
GRANT SELECT ON agents, agent_versions, agent_tasks, agent_task_events TO app_api;
GRANT INSERT ON agents, agent_versions, agent_tasks, agent_task_events TO app_api;
GRANT UPDATE (current_version, status, audience, monthly_budget_micros, max_concurrent) ON agents TO app_api;
GRANT UPDATE (status, finished_at) ON agent_tasks TO app_api;
GRANT SELECT, INSERT, DELETE ON agent_queue TO app_api;

-- Agents work in the background on behalf of the person who started a task, so the worker records
-- that person's usage and checks their allowance, within one firm at a time.
CREATE POLICY worker_read ON usage_events FOR SELECT TO app_worker USING (tenant_id = app_tenant());
CREATE POLICY worker_append ON usage_events FOR INSERT TO app_worker WITH CHECK (tenant_id = app_tenant());
GRANT SELECT, INSERT ON usage_events TO app_worker;
GRANT USAGE ON SEQUENCE usage_events_id_seq TO app_worker;
