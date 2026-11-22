-- When the cache was last opened, so a run can say how long the cache sat
-- unused before it.
CREATE TABLE opened (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  at INTEGER NOT NULL
);
PRAGMA user_version = 17;
