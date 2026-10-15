# Release notes and source discovery

PR publication resolves each dependency's upstream from its ecosystem's own
metadata. npm uses the target version's `repository` (string or URL/directory
object), falling back to `homepage`. Cargo uses crates.io's `repository`; PyPI
uses case-insensitive Source/Source Code/Repository/Code/GitHub/Changelog project
URLs. GitHub Go module paths identify the repository and optional module
subdirectory; `golang.org/x` uses the official GitHub mirrors. Other Go paths use
`?go-get=1`, checking the authoritative import prefix when it differs from the
requested path. No package code or plugin is executed.

Docker/OCI reads the selected image's config labels
`org.opencontainers.image.source` and `org.opencontainers.image.version`.
Multi-platform images prefer Linux amd64, then a known platform; this metadata
read does not change the pinned index digest. Only the two labels enter the
HTTP cache. Official Docker Hub images without a usable source label fall back
to `crates/datasource/src/upstreams.json`; the same data file maps mise core
tools to their upstreams. Other mise backends use the published mise registry
identities. Discovered non-GitHub repositories remain explicit: **source found,
unsupported host**. Unresolved repositories and missing upstream text are
reported separately.

## Range discovery and cost

Only PRs that miss receipt reuse and enter publication read notes. Steady write
runs still perform one authoritative inventory query; they make no metadata,
mirror, release-body or changelog requests. The receipt intent includes the
renderer version so pre-existing PR bodies refresh once after this upgrade.
Publication also reuses an existing body's notes when the table, ranges and
rendering policy are unchanged, so a rebase or label repair does not read notes.
An explicit checked recheck requests fresh rendering.

Release-note tag selection uses the public
[mise-versions GitHub lists](https://github.com/jdx/mise-versions/blob/main/docs/github-versions.md):
`data/github/{owner}/{repo}/releases.json`, with lowercase identities, plus
`tags.json` when comparison endpoints are missing from the release list. Lists
use their upstream ETag/cache-control headers. These are note-discovery reads,
not a replacement for dependency lookup catalogs. Asset counts do not filter
release notes. Mirror tag commit dates never enter release-age decisions.

The current mirror contract says **no direct GitHub catalog fallback on any
error**, including 429/5xx, 403/404, empty answers and truncated lists. This is
stricter than the initial suggestion of falling back only on transport errors.
The implementation follows the published contract. Incomplete results are
visible in the PR. Exact release bodies still come from GitHub's conditional
`releases/tags/{tag}` endpoint for the selected range only.

Versions are selected in `(current, target]`, using the observed current version
where available. Docker variants/digests are removed for upstream version
comparison. Release drafts are excluded; prereleases require a prerelease target.
Plain/v-prefixed tags, package/monorepo namespaces and the Go, jq, bun, nginx and
PostgreSQL upstream spellings are supported. Comparison links retain exact
observed tags. If an endpoint is absent from the mirror, the link follows an
observed upstream tag's spelling and is labelled **inferred tags**; it is not
claimed as verified. Without an observed namespace no comparison URL is invented.

Releases with missing/empty bodies fall back to root or package-local changelogs
named CHANGELOG, CHANGES, HISTORY, RELEASES or NEWS, with common Markdown/text/RST
extensions and casing. Small Contents directory listings discover files in
common documentation directories, including Node's
`doc/changelogs/CHANGELOG_V22.md`. This avoids recursively downloading the entire
upstream source tree. Package-local changelogs take precedence over root files.

Each changelog is parsed in one linear pass, with ATX/setext heading boundaries,
fenced-code handling, optional v prefixes and dates. Redis's 00-RELEASENOTES and
nginx's plain-text release headings are also understood. Parsed sections are
cached in SQLite by repository, path and immutable blob SHA. Batch-shared locks
and parsed section objects coalesce concurrent reads, and other ranges reuse the
same object. A changed blob is parsed once again; an unchanged blob is neither
downloaded nor parsed again, even after restarting the client.

## Rendering and deliberate choices (departure 48)

The body retains the dependency table and ownership/recheck markers within a
32,000-byte budget, conservatively below GitHub's PR-body character limit. Each
dependency gets at most 12,000 bytes; at most eight distinct source/package/range
combinations are rendered. Versions are newest first. After the three newest
versions, older notes appear in `<details>`. Truncation closes our details block
and adds an explicit upstream link. Release-body reads stop after 12,000 bytes
or 32 candidate releases, since older bodies cannot fit. Notes that cannot be
fetched or do not fit are explicitly marked truncated/incomplete. This is a
bounded summary of the range, not a promise to embed unlimited upstream history.

Every `@` outside URL tokens in upstream text gains a U+200D zero-width joiner
immediately after it. Names remain readable while user/team mentions are
neutralised. Table cells and policy notes use the same mention treatment.
Bare `#123` becomes `https://github.com/UPSTREAM/issues/123`, and
`owner/repo#123` becomes that repository's full issue URL. Existing URL tokens
are preserved. HTML comment delimiters are escaped so upstream text cannot
forge rustovate ownership markers. These choices intentionally change upstream
Markdown to avoid notification and cross-reference noise.

## Validation and current limits

`crates/datasource/tests/notes.rs` uses independently written offline metadata
and exercises every source, monorepo namespaces, release/changelog merging,
prereleases, Node major files, conditional responses, changed blobs, a document
larger than 10 MB, byte budgets and notification neutralisation. Public cassettes
cover registry source metadata and the sandbox ranges. Refresh through mise
with `GH_TOKEN=$(gh auth token)` and `RUSTOVATE_RECORD=1`, then run
`mise run fixtures:check`; normal tests never construct a network client.

Upstream websites and non-heading formats such as PostgreSQL SGML and Go's
website-only release documents need explicit adapters in a later change. A
resolved GitHub repository with no supported text still gets source/compare
links and an honest missing-text explanation. The general mise tool TOML/index
catalog migration is also deferred: it must preserve release-age provenance,
coverage markers and current version/digest behavior before replacing lookup.
