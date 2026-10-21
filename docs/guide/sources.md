# Sources

A source is where versions come from. Each dependency has one, chosen by its
manager, and rules can select by it.

| Source | Package identity | Where versions come from |
|---|---|---|
| `npm` | package name | the npm registry, full metadata for publication times, dist-tags and deprecations |
| `crates` | crate name | the crates.io sparse index, with publication times; yanked releases are skipped |
| `go` | module path | the Go module proxy (`@v/list`, `.info` for new versions only) |
| `pypi` | normalised name | the PyPI JSON API, falling back to the PEP 691 simple API |
| `docker` | image | registry v2 tags and manifests on Docker Hub, GHCR and other registries |
| `oci` | `registry/chart` | OCI registry tags for Helm charts |
| `helm` | `https://repo\|chart` | the repository's `index.yaml` |
| `terraform` | `namespace/provider` or `namespace/module/system` | the public Terraform registry |
| `github-releases`, `github-tags` | `owner/repo` | GitHub, through mise-versions when it can |
| `github-runners` | runner family | a catalog of GitHub-hosted runner labels |
| `forge-tags` | `host/path` | GitLab and Gitea/Forgejo tags |
| `mise` | tool name | mise's published registry and the mise-versions index |
| `maven` | `group:artifact` | `maven-metadata.xml` on Maven Central, Google Maven or the Gradle plugin portal |
| `nuget` | package ID | the nuget.org flat container and registration pages |
| `rubygems` | gem name | the RubyGems versions API |
| `packagist` | `vendor/name` | Packagist's p2 metadata |

## Fetching less

Every response is stored with its validators, and every repeat request is
conditional. Unauthenticated registries honour their declared `max-age`. Go proxy
lists and validator-free OCI tag pages get a one-hour local freshness floor
(`--go-freshness-secs`, `--registry-freshness-secs`; `0` restores upstream
behaviour), because those endpoints would otherwise be read on every run.

GitHub histories are where Renovate spent most of its time. The Count reads
them in pieces:

- A cold release history is fetched through batched GraphQL queries, sixteen
  sources at a time.
- A warm one starts with a conditional REST read of the newest releases. Only
  the fields that matter (tag, date, draft, prerelease) feed the fingerprint,
  so a download counter changing does not trigger a refetch.
- When the head did change, history is read newest first and merged into the
  cache until a known release turns up.
- Tags use one conditional read of the matching refs, then hydrate only new or
  moved tags.

A history-head check cannot see a release deleted or edited behind an
unchanged head. Webhook invalidation or periodic reconciliation covers that.

## mise-versions host

By default (`useVersionsHost = true`), public GitHub repositories that appear in
mise's registry are read from the public
[mise-versions](https://mise-versions.jdx.dev) mirror instead of the GitHub API:
`releases.json` and `tags.json`, validated by ETag. Repositories outside the
registry, including private ones, go straight to GitHub. If the mirror fails
with a transport error, 429 or 5xx, the Count falls back to GitHub; a 403, 404 or
empty list does not, and a truncated list is reported as an incomplete lookup.
mise tools read the mise-versions index once per batch. Set
`useVersionsHost = false` to read everything upstream.

## Registries and limits

Docker Hub and GHCR use anonymous registry tokens, cached until shortly before
they expire. To use authenticated Docker Hub pulls, set both
`PATCHULA_DOCKERHUB_USERNAME` and `PATCHULA_DOCKERHUB_TOKEN`; they are only ever
sent to Docker Hub's token service. When Docker Hub's `ratelimit-remaining` hits
zero, queued reads stop. A 429 shares a cooldown across the host and retries at
most three times, honouring `Retry-After`. If the wait is too long, the last
cached response is used and marked stale in the report; a cold miss is still an
error.

Redirects are followed up to five times, HTTPS only, with credentials stripped
when the origin changes.

## Dates and release age

Publication dates feed the [release-age wait](/guide/lookup-policy). Go dates
releases through the proxy's `.info` documents, only for the 20 newest on a cold
lookup, since a module can list hundreds and older releases are outside any
wait; versions the proxy lists but cannot describe are dropped. npm, crates,
PyPI, RubyGems, NuGet and Packagist report them directly. Maven Central's search
API lags by months, so Maven uses each POM's `Last-Modified` header, for the ten
newest versions only. GitHub tags use the release date, then the annotated tag
date. mise tools use mise's own first-seen date. Lightweight tags, Gitea tags
and hosted runner labels are undated and have no wait.

## Where the upstream lives

For release notes, each dependency's source repository comes from its
ecosystem's own metadata: npm's `repository` or `homepage`, crates.io's
`repository`, PyPI's project URLs, Go module paths (with `?go-get=1` for vanity
paths), and Docker's `org.opencontainers.image.source` label. See [release
notes](/guide/release-notes).
