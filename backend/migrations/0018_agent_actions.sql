-- Things an agent has prepared for a person to do themselves: an email to send or a meeting to
-- schedule. The agent never sends or books anything. The person reviews the exact text, may edit
-- it, and approves; their own mail or calendar then opens with it filled in, and they press Send.
-- No mailbox or calendar is connected for this, so nothing here has any access to either.

CREATE TABLE agent_actions (
  id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id    uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  task_id      uuid NOT NULL,
  -- Whose it is: the person the task ran for. Nobody else can see it.
  requested_by text NOT NULL,
  kind         text NOT NULL CHECK (kind IN ('email', 'meeting')),
  payload      jsonb NOT NULL,
  -- The person changed what the agent wrote before approving it.
  edited       boolean NOT NULL DEFAULT false,
  status       text NOT NULL DEFAULT 'proposed' CHECK (status IN ('proposed', 'approved', 'dismissed')),
  created_at   timestamptz NOT NULL DEFAULT now(),
  decided_at   timestamptz,
  FOREIGN KEY (tenant_id, task_id) REFERENCES agent_tasks (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX agent_actions_waiting ON agent_actions (tenant_id, requested_by, created_at) WHERE status = 'proposed';
CREATE INDEX agent_actions_by_task ON agent_actions (tenant_id, task_id);

ALTER TABLE agent_actions ENABLE ROW LEVEL SECURITY; ALTER TABLE agent_actions FORCE ROW LEVEL SECURITY;
REVOKE ALL ON agent_actions FROM PUBLIC;
GRANT SELECT, INSERT, UPDATE, DELETE ON agent_actions TO app_worker;
CREATE POLICY tenant_isolation ON agent_actions FOR ALL TO app_api, app_worker
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());

-- The agent's worker creates proposals. The API reads them and records the person's decision and
-- edits; it cannot create one.
GRANT SELECT ON agent_actions TO app_api;
GRANT UPDATE (payload, edited, status, decided_at) ON agent_actions TO app_api;
