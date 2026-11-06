-- Consecutive failed runs of a queued repository, which set its retry backoff.
ALTER TABLE server_work ADD COLUMN failures INTEGER NOT NULL DEFAULT 0;
PRAGMA user_version = 9;
