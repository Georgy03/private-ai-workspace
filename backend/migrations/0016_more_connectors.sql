-- Dropbox and DocuSign join Google Drive and Microsoft 365 as kinds of connection.
DO $$
DECLARE c text;
BEGIN
  FOR c IN SELECT conname FROM pg_constraint
           WHERE conrelid = 'connections'::regclass AND contype = 'c' AND pg_get_constraintdef(oid) LIKE '%microsoft365%'
  LOOP
    EXECUTE format('ALTER TABLE connections DROP CONSTRAINT %I', c);
  END LOOP;
END $$;
ALTER TABLE connections
  ADD CONSTRAINT connections_kind_check CHECK (kind IN ('microsoft365', 'google_drive', 'dropbox', 'docusign'));
