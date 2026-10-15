-- NULL means an unconditional repo trigger; otherwise retain abbreviated check
-- event PR numbers for asynchronous ownership verification before running.
ALTER TABLE server_work ADD COLUMN check_prs TEXT;
PRAGMA user_version = 5;
