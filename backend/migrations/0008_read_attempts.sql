-- How many times in a row reading a file failed in a way that might not repeat (the reader timed
-- out or crashed). A file that keeps doing so is given up on instead of being retried forever.
ALTER TABLE documents ADD COLUMN read_attempts integer NOT NULL DEFAULT 0;
