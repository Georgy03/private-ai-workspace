-- Saving a file to Google Drive no longer has its own connection or its own switch: the one Google
-- Drive connection asks for both permissions at sign-in. Remove what 0020 added for it.

DELETE FROM connections WHERE kind = 'google_drive_save';
ALTER TABLE connections DROP CONSTRAINT connections_kind_check;
ALTER TABLE connections
  ADD CONSTRAINT connections_kind_check CHECK (kind IN ('microsoft365', 'google_drive', 'dropbox', 'docusign', 'outlook_mail', 'quickbooks'));
ALTER TABLE firm_settings DROP COLUMN allow_drive_saving;
