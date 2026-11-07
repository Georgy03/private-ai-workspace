-- Zoom, connected for one person only so that meetings they approve can be created in their own
-- Zoom account. It reads no meetings or recordings and is never synced.

ALTER TABLE connections DROP CONSTRAINT connections_kind_check;
ALTER TABLE connections
  ADD CONSTRAINT connections_kind_check CHECK (kind IN ('microsoft365', 'google_drive', 'dropbox', 'docusign', 'outlook_mail', 'quickbooks', 'zoom'));
