-- Add host column to accounts for JMAP/IMAP host storage.

ALTER TABLE accounts ADD COLUMN host TEXT NOT NULL DEFAULT '';
