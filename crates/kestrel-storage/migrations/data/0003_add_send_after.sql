-- Add send_after column for scheduled send support.
ALTER TABLE outbox ADD COLUMN send_after INTEGER;
