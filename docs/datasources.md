# Sources (SWE-97)

npm requests full metadata (`application/json`) for publication timestamps,
retaining dist-tags and deprecated flags. Versioned compact catalogs retain the fields used by policy; fresh hits and
304s avoid decoding the original document.
crates.io reads sparse-index pubtime and excludes yanked releases during policy.
Go uses escaped module paths, @v/list and .info for new versions only. It skips
@latest when the list supplies tagged versions, preferring stable releases; an
empty list still queries @latest for a default-branch pseudo-version. Retraction
metadata is not implemented (as before).
All unauthenticated registries honor declared max-age freshness, including
responses with validators, unless no-cache is present. Expired responses use
validators when available; 304 metadata refreshes do not rewrite compressed bodies.
Go proxy lists/latest and validator-free OCI tag pages have configurable
one-hour local freshness windows; see [cache policy](cache.md#local-freshness-and-host-limits).
These explicitly depart from upstream freshness. npm and sparse
indexes are aggregate upstream responses, not paginated version endpoints.

GitHub uses the batch governor. Cold release sources share GraphQL queries in
groups of at most sixteen, including their first hundred releases. Warm sources
use parallel conditional REST head reads, comparing only tag, publication date,
draft and prerelease fields. Asset download counters, author profiles and
reactions never affect the fingerprint, so a counter-driven 200 does not fetch
history. Changed heads refresh newest-first history, merging into SQLite until
a known release. A live A/B experiment found warm GraphQL batches slower than
parallel conditional REST, so batching is reserved for cold first pages.

Tags use a conditional REST matching-refs namespace read, followed by bounded
parallel repository-scoped GraphQL object queries for new or retargeted refs.
Tags sharing an immutable target share one hydration/cache entry. Annotated tags
peel to commits. Per-target data and complete catalog fingerprints avoid warm
hydration, including when a namespace responds 200 with identical semantic data.
This detects older tag additions, retargeting and deletion anywhere in the
namespace. The revalidated catalog supplies action tag digests; opaque branch
refs still use conditional commit lookups.

Release-head gates cannot detect historical release deletion/metadata edits or
insertion behind an unchanged head. These still need webhook invalidation or
periodic reconciliation; exhaustive release-history reconciliation is not claimed.

mise short names resolve through the published registry snapshot in
`crates/mise/registry.json` (`mise run registry:update`). Supported backends are
aqua/github/ubi/packslip (GitHub releases), npm, cargo, pypi, and supported core
tools. Unsupported plugins are not executed. Node uses its official distribution
index; Bun uses oven-sh/bun releases and strips bun-v tags; Go uses golang/go
tags and strips go; Python uses CPython tags; Rust uses rust-lang/rust releases.
Unknown tools report a warning. This resolves catalog identities, not custom
tool_alias settings, backend platform constraints, or plugin-defined version logic.
PyPI retains earliest file upload dates and all-files-yanked status. Python
requirements use PEP 440 ordering/specifiers; mise keeps its loose version policy.

Wave 2 adds anonymous registry v2 authentication for Docker Hub and GHCR, bounded
same-origin tag pagination, conditional pages, and lazy HEAD manifest/index digest
resolution for current/proposed tags. Anonymous pull tokens coalesce in memory
and persist in SQLite until their scoped expiry minus a safety margin. GitHub
and private registry credentials are never persisted as tokens. Registry 429s share a host cooldown and retry at most three times, honoring
Retry-After (up to 60 seconds per retry) or backing off 1/2/4 seconds when absent.
Longer waits/exhaustion are explicit errors; failed pages are never cached. Lexically
ordered tag pages cannot safely stop at a known version; full enumeration detects
new historical tags. Numeric Docker updates keep the exact suffix variant.
OCI charts use registry tags/manifests, translating SemVer build metadata to `_`.

Terraform identities are `namespace/provider` or `namespace/module/system`, using
the public registry versions APIs and compact derived catalogs. Helm identities
are `https://repository|chart`; conditional `index.yaml` responses are compacted
to per-chart catalogs, omitting URLs, descriptions and chart metadata. PyPI JSON
falls back to PEP 691 JSON on 404/406; `pypi_simple` is also directly callable.
The simple API groups wheel/sdist files by version and retains yank/upload data.

Specifications: [registry v2](https://distribution.github.io/distribution/spec/api/),
[token authentication](https://distribution.github.io/distribution/spec/auth/token/),
[Terraform registry](https://developer.hashicorp.com/terraform/registry/api-docs),
[Helm index](https://helm.sh/docs/topics/chart_repository/),
[PEP 691](https://packaging.python.org/en/latest/specifications/simple-repository-api/),
[PEP 440](https://packaging.python.org/en/latest/specifications/version-specifiers/).

Hosted runner versions are maintained in `crates/datasource/src/runners.json`,
checked against [GitHub's runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
on 2026-10-15. Edit that data file and run CI to refresh support. Families are
separate for arm, Intel and Visual Studio variants; proposals never cross them.
The catalog tracks standard ubuntu, macos and windows labels. Custom larger
runners, xcode preview labels and ubuntu-slim are not version upgrade families.
The docs do not supply release timestamps, so runner versions have no age gate.

Public recordings use tiny-tarball, heck, rsc.io/quote, jdx/mr-boxington and
GitHub's hello-world action. Fresh registry responses reduce the warm source run
to three GitHub requests: three GETs (release head, tag namespace and moving ref); the first warm release
head primes its REST validators, and unchanged subsequent reads can be 304. No
.info or GraphQL history requests occur.

References: [npm metadata](https://github.com/npm/registry/blob/main/docs/responses/package-metadata.md),
[Cargo index](https://doc.rust-lang.org/cargo/reference/registry-index.html),
[Go proxy](https://go.dev/ref/mod#goproxy-protocol),
[GitHub GraphQL](https://docs.github.com/en/graphql/reference).
Tag enumeration follows [matching references](https://docs.github.com/en/rest/git/refs#list-matching-references).
Additional specifications: [Node distribution index](https://nodejs.org/dist/index.json),
[PyPI JSON API](https://docs.pypi.org/api/json/),
[mise registry format](https://mise.jdx.dev/contributing/registry.html).
