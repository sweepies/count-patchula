-- Compact bodies have an explicit versioned representation in their HTTP key.
-- Old raw registry bodies cannot satisfy those keys; discard them and obsolete
-- content-hashed npm extractions instead of retaining megabytes forever.
DELETE FROM http WHERE json_extract(key, '$.url') LIKE 'https://registry.npmjs.org/%'
    OR json_extract(key, '$.url') LIKE 'https://index.crates.io/%'
    OR json_extract(key, '$.url') LIKE 'https://pypi.org/pypi/%';
DELETE FROM http WHERE json_extract(key, '$.url') LIKE '%/releases?per_page=1';
DELETE FROM extraction WHERE repo LIKE 'catalog:npm:%';
DELETE FROM releases WHERE source IN ('npm', 'crates');
DELETE FROM release_heads WHERE source IN ('npm', 'crates');
PRAGMA user_version = 3;
