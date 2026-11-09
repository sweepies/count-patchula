-- Release catalogs now drop GitHub-flagged prereleases, but lists stored by
-- the previous release still hold them and merge_releases never removes
-- rows. Drop the github-releases lists and heads with the source-head
-- fingerprints that gate their rebuild, so the next refresh rewrites them.
DELETE FROM releases WHERE source = 'github-releases';
DELETE FROM release_heads WHERE source = 'github-releases';
DELETE FROM state
WHERE namespace = 'source-head@v1'
  AND json_extract(scope, '$.source') = 'github-releases';
PRAGMA user_version = 11;
