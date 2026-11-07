-- What a check of a finished report against the text its agent actually read found: how many
-- figures and quotations were there, and which could not be found. The worker writes it; the
-- person who asked reads it with the rest of the report.
ALTER TABLE agent_tasks ADD COLUMN checks jsonb;
