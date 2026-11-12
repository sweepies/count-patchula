-- Who waits for the work: a person's trigger runs first (0), then check
-- events (1), then the sweep and discovery (2). Rows already queued cannot
-- be told apart; the sweep's class keeps a new trigger ahead of them.
ALTER TABLE server_work ADD COLUMN priority INTEGER NOT NULL DEFAULT 2;
PRAGMA user_version = 13;
