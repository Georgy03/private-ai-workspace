-- Reconnecting a source whose access was withdrawn re-activates the existing connection instead of
-- adding a second one, so the API may reset its status and last error. It still cannot touch
-- the connection's token name, configuration or cursor.
GRANT UPDATE (status, last_error) ON connections TO app_api;
