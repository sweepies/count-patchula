ALTER TABLE http ADD COLUMN last_access INTEGER NOT NULL DEFAULT 0;
CREATE INDEX http_lru ON http(last_access);
CREATE TABLE access_clock(value INTEGER NOT NULL);
INSERT INTO access_clock(value) VALUES (0);
PRAGMA user_version = 2;
