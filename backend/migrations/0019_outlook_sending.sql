-- Sending the emails an agent prepared, from the person's own Outlook, after they have read them.
-- This is the first thing the server can do that leaves the firm, so it is off until an
-- administrator allows it, limited per person per day, and each email is sent exactly once.

ALTER TABLE connections DROP CONSTRAINT connections_kind_check;
ALTER TABLE connections
  ADD CONSTRAINT connections_kind_check CHECK (kind IN ('microsoft365', 'google_drive', 'dropbox', 'docusign', 'outlook_mail'));
-- What to call a connection: for a mailbox, the address it sends from.
ALTER TABLE connections ADD COLUMN label text;
GRANT SELECT (label) ON connections TO app_api;

ALTER TABLE firm_settings ADD COLUMN allow_email_sending boolean NOT NULL DEFAULT false;

ALTER TABLE agent_actions DROP CONSTRAINT agent_actions_status_check;
ALTER TABLE agent_actions
  ADD CONSTRAINT agent_actions_status_check CHECK (status IN ('proposed', 'approved', 'dismissed', 'sent')),
  ADD COLUMN sent_at timestamptz,
  ADD COLUMN sent_from text;
GRANT UPDATE (sent_at, sent_from) ON agent_actions TO app_api;
