-- Typed state: each value type owns a namespaced, versioned row, so a change
-- to a stored shape or its meaning never reuses entries written for the old
-- one. The extraction table's hand-keyed rows are dropped with it.
CREATE TABLE state (
    namespace TEXT NOT NULL,
    scope TEXT NOT NULL,
    value BLOB NOT NULL,
    PRIMARY KEY(namespace, scope)
);
DROP TABLE extraction;
PRAGMA user_version = 10;
