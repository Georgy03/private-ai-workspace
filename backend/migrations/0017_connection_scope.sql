-- Who a connection's files are for. "workspace": everyone at the firm can search them, each person
-- seeing only the files the provider says they may open (how connections always worked).
-- "private": only the person who connected it can find any of them, whatever the provider says
-- about sharing. Any employee may connect a private source; only an administrator may connect one
-- for the whole workspace.

ALTER TABLE connections
  ADD COLUMN visibility text NOT NULL DEFAULT 'workspace' CHECK (visibility IN ('workspace', 'private')),
  ADD COLUMN owner text;
UPDATE connections SET owner = config ->> 'connected_by' WHERE owner IS NULL;
-- A private connection must say whose it is.
ALTER TABLE connections ADD CONSTRAINT connections_private_has_owner CHECK (visibility = 'workspace' OR owner IS NOT NULL);

GRANT SELECT (visibility, owner) ON connections TO app_api;
-- The owner changes who a connection is for; the API checks who may.
GRANT UPDATE (visibility) ON connections TO app_api;

-- Changing a connection's scope takes effect in the same moment, in the same transaction:
--  * to private: every file it holds becomes readable by its owner alone;
--  * to workspace: files stay owner-only until the next sync restores each file's real sharing,
--    which a full re-listing (cursor cleared) does straight away.
-- The function acts only inside the firm of the row that changed.
CREATE FUNCTION connection_scope_changed() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path = public AS $$
BEGIN
  IF NEW.visibility = 'private' THEN
    UPDATE documents SET readable_by = ARRAY['user:' || NEW.owner]
      WHERE connection_id = NEW.id AND tenant_id = NEW.tenant_id;
  ELSE
    UPDATE connections SET cursor = NULL WHERE id = NEW.id AND tenant_id = NEW.tenant_id;
  END IF;
  RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION connection_scope_changed() FROM PUBLIC;
CREATE TRIGGER connection_scope_changed AFTER UPDATE OF visibility ON connections
  FOR EACH ROW WHEN (OLD.visibility IS DISTINCT FROM NEW.visibility)
  EXECUTE FUNCTION connection_scope_changed();
