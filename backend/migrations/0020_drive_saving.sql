-- Saving a file into the person's own Google Drive, after they have read it and pressed approve.
-- A separate connection from the one that reads Drive: it asks Google for the narrow "files this
-- app created" permission (drive.file), so it can add files but cannot read, change or delete any
-- file it did not make. Off until an administrator allows it.

ALTER TABLE connections DROP CONSTRAINT connections_kind_check;
ALTER TABLE connections
  ADD CONSTRAINT connections_kind_check CHECK (kind IN ('microsoft365', 'google_drive', 'dropbox', 'docusign', 'outlook_mail', 'google_drive_save'));

ALTER TABLE firm_settings ADD COLUMN allow_drive_saving boolean NOT NULL DEFAULT false;
