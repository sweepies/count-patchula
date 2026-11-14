-- The derived representation an HTTP key names, indexed, so the prune on
-- open finds a representation's superseded entries without scanning every
-- key. Every key is the JSON a `HttpKey` serializes to.
ALTER TABLE http ADD COLUMN representation TEXT
  GENERATED ALWAYS AS (json_extract(key, '$.representation')) VIRTUAL;
CREATE INDEX http_representation ON http(representation)
  WHERE representation IS NOT NULL;
PRAGMA user_version = 15;
