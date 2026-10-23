-- What the server last saw of each repository it runs, for its read-only API.
-- `summary` and `snapshot` are server-serialized JSON, NULL until a run.
CREATE TABLE server_repositories (
    repo TEXT PRIMARY KEY,
    installation INTEGER NOT NULL CHECK (installation > 0),
    private INTEGER NOT NULL CHECK (private IN (0, 1)),
    ran_at INTEGER,
    summary BLOB,
    snapshot BLOB
);
CREATE INDEX server_repositories_installation ON server_repositories (installation);
PRAGMA user_version = 8;
