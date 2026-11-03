-- Where a person opens the original file (Google Drive or SharePoint/OneDrive). Null until the
-- next time the file is seen; Google Drive links can also be built from the file's ID.
ALTER TABLE documents ADD COLUMN web_url text;
