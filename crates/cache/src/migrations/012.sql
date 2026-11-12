-- Consecutive requeues of a queued repository; past a cap the next one is a
-- failure, so a cause that never resolves cannot rerun every minute forever.
ALTER TABLE server_work ADD COLUMN requeues INTEGER NOT NULL DEFAULT 0;
PRAGMA user_version = 12;
