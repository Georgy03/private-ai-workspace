-- How long agent work is kept. A report and the conversation behind it are kept for the firm's
-- retention period (90 days unless the firm chooses otherwise), then deleted. A few days before,
-- the person who owns it is asked whether to keep it; their answer is final.

ALTER TABLE firm_settings
  -- Null means the default of 90 days; 0 means keep until the owner deletes it.
  ADD COLUMN agent_retention_days integer CHECK (agent_retention_days IS NULL OR agent_retention_days BETWEEN 0 AND 3650);

ALTER TABLE agent_tasks
  ADD COLUMN retention_until  timestamptz,
  -- 'keep': kept until the owner deletes it. 'expire': the owner was asked and let it run out.
  ADD COLUMN retention_choice text CHECK (retention_choice IN ('keep', 'expire')),
  ADD COLUMN deleted_at       timestamptz;

-- Which documents' text a stored message holds, so it can be dropped if the person later loses
-- access to one of them.
ALTER TABLE agent_task_messages ADD COLUMN documents uuid[] NOT NULL DEFAULT '{}';

CREATE FUNCTION agent_retention_until() RETURNS timestamptz LANGUAGE sql STABLE AS $$
  SELECT CASE WHEN d = 0 THEN NULL ELSE now() + make_interval(days => d) END
  FROM (SELECT COALESCE((SELECT agent_retention_days FROM firm_settings), 90) AS d) s
$$;

-- When each task's work is due to be deleted. Like the other queues it holds only IDs and times,
-- so the worker can find due work across firms without reading any firm's data.
CREATE TABLE agent_purge_queue (
  task_id       uuid PRIMARY KEY,
  tenant_id     uuid NOT NULL,
  purge_at      timestamptz NOT NULL,
  claimed_until timestamptz,
  FOREIGN KEY (tenant_id, task_id) REFERENCES agent_tasks (tenant_id, id) ON DELETE CASCADE
);
ALTER TABLE agent_purge_queue ENABLE ROW LEVEL SECURITY; ALTER TABLE agent_purge_queue FORCE ROW LEVEL SECURITY;
REVOKE ALL ON agent_purge_queue FROM PUBLIC;
GRANT SELECT, INSERT, UPDATE, DELETE ON agent_purge_queue TO app_api, app_worker;
CREATE POLICY own_firm ON agent_purge_queue FOR ALL TO app_api
  USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant());
CREATE POLICY every_firm ON agent_purge_queue FOR ALL TO app_worker USING (true) WITH CHECK (true);

-- Keeps the queue in step with the task, whoever changes it.
CREATE FUNCTION agent_purge_sync() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.retention_until IS NULL OR NEW.deleted_at IS NOT NULL THEN
    DELETE FROM agent_purge_queue WHERE task_id = NEW.id;
  ELSE
    INSERT INTO agent_purge_queue (task_id, tenant_id, purge_at) VALUES (NEW.id, NEW.tenant_id, NEW.retention_until)
    ON CONFLICT (task_id) DO UPDATE SET purge_at = EXCLUDED.purge_at, claimed_until = NULL;
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER agent_purge_sync AFTER INSERT OR UPDATE OF retention_until, deleted_at ON agent_tasks
  FOR EACH ROW EXECUTE FUNCTION agent_purge_sync();

-- Work already finished gets the default period, with at least a week's notice.
UPDATE agent_tasks SET retention_until = GREATEST(finished_at + interval '90 days', now() + interval '7 days')
  WHERE finished_at IS NOT NULL AND retention_until IS NULL;

-- A person decides about their own reports. Nothing else about a task is theirs to write.
GRANT UPDATE (retention_until, retention_choice) ON agent_tasks TO app_api;
