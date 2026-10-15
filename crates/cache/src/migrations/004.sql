-- Queue rows survive the entire execution. Generation fencing preserves events
-- arriving while an older generation is in flight. NULL repo represents discovery.
CREATE TABLE server_work (
    installation INTEGER NOT NULL,
    repo TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 1,
    claimed INTEGER,
    available_at INTEGER NOT NULL,
    PRIMARY KEY (installation, repo)
);
CREATE TABLE server_deliveries (
    delivery TEXT PRIMARY KEY,
    received_at INTEGER NOT NULL
);
PRAGMA user_version = 4;
