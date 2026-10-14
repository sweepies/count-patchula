CREATE TABLE http (
    key TEXT PRIMARY KEY,
    status INTEGER NOT NULL,
    etag TEXT,
    last_modified TEXT,
    headers TEXT NOT NULL,
    body BLOB NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER,
    cache_control TEXT
);
CREATE TABLE release_heads (
    source TEXT NOT NULL,
    package TEXT NOT NULL,
    newest TEXT NOT NULL,
    PRIMARY KEY(source, package)
);
CREATE TABLE releases (
    source TEXT NOT NULL,
    package TEXT NOT NULL,
    version TEXT NOT NULL,
    published_at TEXT,
    digest TEXT,
    position INTEGER NOT NULL,
    PRIMARY KEY(source, package, version)
);
CREATE INDEX release_order ON releases(source, package, position);
CREATE TABLE extraction (
    repo TEXT NOT NULL,
    branch_commit TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    result BLOB NOT NULL,
    PRIMARY KEY(repo, branch_commit, config_hash)
);
PRAGMA user_version = 1;
