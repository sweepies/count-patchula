-- State namespaces and derived representations now carry a fingerprint of
-- their listed crates' locked versions (`name@vN~<fingerprint>`), so the
-- entries written under the plain `name@vN` keys of every fingerprinted
-- name are never read again. Every fingerprinted name at this schema:
-- each row of tests/state-versions.tsv and tests/derived-versions.tsv.
WITH names(name) AS (VALUES
  ('extract'),
  ('files'),
  ('config'),
  ('config-files'),
  ('catalog'),
  ('catalog-controls'),
  ('not-ours'),
  ('changelog'),
  ('notes-adapter'),
  ('source-head'),
  ('tag-catalog'),
  ('tag-object'),
  ('osv-advisory'),
  ('release-notes')
)
DELETE FROM state
WHERE instr(namespace, '~') = 0
  AND substr(namespace, 1, instr(namespace, '@v') - 1) IN (SELECT name FROM names);
WITH names(name) AS (VALUES
  ('npm-catalog-v2'),
  ('crates-catalog-v2'),
  ('pypi-catalog-v2'),
  ('terraform-catalog'),
  ('helm-index'),
  ('pypi-index-catalog'),
  ('pypi-simple-catalog'),
  ('nuget-flat'),
  ('nuget-registration'),
  ('nuget-registration-page'),
  ('maven-metadata'),
  ('packagist-p2'),
  ('rubygems-versions'),
  ('forge-gitlab-tags'),
  ('forge-gitea-tags'),
  ('oci-config-notes'),
  ('oci-tags'),
  ('image-platform'),
  ('sbom-os'),
  ('github-release-head'),
  ('anonymous-registry-pull-token'),
  ('notes-adapter-listing'),
  ('parsed-postgres-sgml'),
  ('parsed-go-history'),
  ('notes-changelog-directory'),
  ('parsed-changelog'),
  ('notes-gitlab-releases'),
  ('notes-gitea-releases'),
  ('notes-forge-tags'),
  ('notes-forge-files'),
  ('notes-forge-changelog'),
  ('notes-source-npm'),
  ('notes-source-crates'),
  ('notes-source-pypi'),
  ('notes-mise-versions')
)
DELETE FROM http
WHERE EXISTS (
  SELECT 1 FROM names
  WHERE instr(http.key, '"representation":"' || names.name || '@v') > 0
)
AND NOT (
  instr(substr(key, instr(key, '"representation":"') + 18), '~') > 0
  AND instr(substr(key, instr(key, '"representation":"') + 18), '~')
    < instr(substr(key, instr(key, '"representation":"') + 18), '"')
);
PRAGMA user_version = 15;
