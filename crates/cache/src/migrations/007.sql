-- App-scoped, expiring repository/organization installation IDs; no credentials.
CREATE TABLE server_installations (
    app INTEGER NOT NULL CHECK (app > 0),
    target TEXT NOT NULL,
    installation INTEGER NOT NULL CHECK (installation > 0),
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (app, target)
);
PRAGMA user_version = 7;
