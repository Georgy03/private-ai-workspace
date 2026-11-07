-- QuickBooks Online as a source: each company's customers, vendors, accounts, invoices, bills,
-- payments, expenses, journal entries and key reports are indexed, readable only by the person who
-- connected the company.

ALTER TABLE connections DROP CONSTRAINT connections_kind_check;
ALTER TABLE connections
  ADD CONSTRAINT connections_kind_check CHECK (kind IN ('microsoft365', 'google_drive', 'dropbox', 'docusign', 'outlook_mail', 'google_drive_save', 'quickbooks'));
