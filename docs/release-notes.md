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
An explicit PR or repository recheck requests fresh rendering. Publication
compares the rendered body hash with fresh PR state before PATCHing; receipts
use the same hash. CRLF and LF are equivalent, so GitHub's newline conversion
does not produce redundant edits. Title/base changes and checked PR boxes still
require reconciliation. Receipt restoration retains its single-inventory fast
path without fetching notes.

A forced recheck still validates branch ownership and signed history. It compares
locally computed Git blob SHAs, modes and deletions against the current branch
tree before creating Git objects. With the same base, the full intended leaf
tree must match, including the absence of edits left over from an older plan.
An identical tree reuses the existing commit even when the retry or policy
fingerprint changed. A moved base still follows the configured rebase policy.
The branch batch shares one viewer lookup; unchanged branches send no tree,
commit or ref mutations.

One publication batch shares source metadata, release lists, tag lists, exact
release bodies and changelog directory reads across ecosystems and version ranges.
Both sequential and concurrent PRs reuse these reads (including failures).
For example, mise, Docker and GitHub updates pointing at Node share overlapping
release-body reads and changelog discovery. Before rendering, publication unions
selected tags across PRs by upstream repository. An aliased GraphQL query fetches
all selected bodies for each upstream, bounded to 100 tags per query. Bodies are
persisted in SQLite by repository and exact tag together with `updatedAt`.
Later batches query only timestamps for cached tags; unchanged bodies are neither
downloaded nor rewritten. Edited releases get one additional batch query for
their new bodies. Missing releases and failures are memoised only within the
batch, and are checked again next time. Immutable changelog blobs retain their
existing persistent parsed-section cache.

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
visible in the PR. Exact releases come from GitHub GraphQL's
`repository { release(tagName: ...) { updatedAt description isDraft isPrerelease } }`
for mirror-selected tags only; there is no GitHub catalog fallback.

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

Upstreams without GitHub releases fall back to one upstream-specific adapter
when the range still lacks sections. The adapter table is data-driven, and each
adapter reads a directory listing to find an immutable blob, then caches the
parsed notes by blob SHA like a changelog. A range that spans two PostgreSQL
majors keeps only the target major's notes and is marked incomplete.

## Rendering and deliberate choices (departure 48)

The body retains the dependency table and ownership/recheck markers within a
32,000-byte budget, conservatively below GitHub's PR-body character limit. Each
dependency gets at most 12,000 bytes; at most eight distinct source/package/range
combinations are rendered. Versions are newest first. After the three newest
versions, older notes appear in `<details>`. Truncation closes our details block
and adds an explicit upstream link. Each range selects at most 32 candidate
releases for fetching and stops rendering after 12,000 bytes. Notes that cannot be
fetched or do not fit are explicitly marked truncated/incomplete. This is a
bounded summary of the range, not a promise to embed unlimited upstream history.

Only user/team mention tokens in upstream prose gain a U+200D zero-width joiner
after `@`. A mention must begin at the start of text, whitespace or punctuation.
Inline code (including multiple-backtick and multiline spans), fenced code,
URLs, email addresses, `name@version`, scoped packages with versions and
`@sha256:` forms are preserved. Bare scoped packages in dependency-release
sentences such as `Bump @actions/core ... by @author` are preserved too; outside
that package context, `@org/team` has GitHub's team-mention syntax. Existing link
labels are preserved verbatim. Generated tables, package headings, policy text,
source/compare links and ownership/control markers never undergo mention
neutralisation. HTML comment delimiters in upstream or untrusted display text
are still escaped to prevent forged bot markers.

Bare `#123` and `owner/repo#123` outside links and code become Markdown links
labelled `owner/repo#123`, using the resolved upstream for local references.
Upstream GitHub issue, pull request, discussion and commit links (including
existing Markdown destinations, reference definitions and autolinks) use
`https://redirect.github.com/...`. GitHub documents this host under
[Avoiding backlinks to linked references](https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/autolinked-references-and-urls#avoiding-backlinks-to-linked-references):
it prevents cross-reference backlinks, and also removes hover previews. This
deliberate departure avoids upstream timeline noise from public update PRs.
GitHub notes that this mechanism is unavailable for `ghe.com`; rustovate's
rewrite is limited to `github.com` reference URLs. Source, compare, release and
changelog links retain their original hosts, as do URLs shown inside code.

The 2026-10-15 sandbox audit found three corrupted jq code tokens in PR #8
(`@uri`, `@urid`, `@base64d`), plus two checkout commit pins and two scoped npm
package names in PR #12. At the time of inspection these bodies had 79 and 61
joiners respectively; the remaining joiners were author mentions. Offline
regressions cover these cases and each exclusion above without storing the live
PR bodies as fixtures.

## Validation and current limits

`crates/datasource/tests/notes.rs` uses independently written offline metadata
and exercises every source, monorepo namespaces, release/changelog merging,
prereleases, Node major files, conditional responses, changed blobs, a document
larger than 10 MB, byte budgets and notification neutralisation. Public cassettes
cover registry source metadata and the sandbox ranges. Existing REST release
captures remain verbatim; offline tests explicitly project their fields into
simulated GraphQL responses. These are not recorded GraphQL evidence. Independent
fixtures assert alias batching, timestamp-only warm queries, changed/deleted
release handling and cache reuse after reopening SQLite. Refresh through mise
with `GH_TOKEN=$(gh auth token)` and `RUSTOVATE_RECORD=1`, then run
`mise run fixtures:check`; normal tests never construct a network client.

Upstreams that publish their notes outside GitHub releases have explicit
adapters. PostgreSQL keeps its release notes as SGML in
`doc/src/sgml/release-{major}.sgml`, which exists only on that major's branch:
the adapter reads the file at the target version's tag, turns paragraphs, list
items, section titles and inline tags into compact Markdown, and links every
release to its documentation page. Go publishes its release history as data in
the website repository's `internal/history/release.go`: the adapter parses the
release entries into the one-sentence summary the website renders, and links
every release to the release history page. Both adapters read an immutable
blob, cache the parsed notes by blob SHA and report incomplete notes when the
upstream text cannot be read. A resolved GitHub repository with no supported
text still gets source/compare links and an honest missing-text explanation.
The general mise tool TOML/index catalog migration is also deferred: it must
preserve release-age provenance, coverage markers and current version/digest
behavior before replacing lookup.
