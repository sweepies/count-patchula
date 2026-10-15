# Sources (SWE-97)

npm requests full metadata (`application/json`) for publication timestamps,
retaining dist-tags and deprecated flags. Parsed catalogs are cached by body
fingerprint so unchanged bodies skip JSON parsing/sorting and release merging.
crates.io reads sparse-index pubtime and excludes yanked releases during policy.
Go uses escaped module paths, @v/list, @latest and .info for new versions only.
All unauthenticated registries honor declared max-age freshness, including
responses with validators, unless no-cache is present. Expired responses use
validators when available; 304 metadata refreshes do not rewrite compressed bodies.
Go proxy responses lack validators; aggregate list/latest requests refresh on
expiry. npm and sparse
indexes are aggregate upstream responses, not paginated version endpoints.

GitHub uses the batch governor. Conditional REST head probes gate newest-first
GraphQL releases and tag connections; refreshed pages merge into SQLite and stop
at the first known release. A warm unchanged head issues no GraphQL request.
Tag targets peel annotated tags to commits; conditional commit/ref lookups keep
moving branch pins and retargeted version tags fresh.

The REST head gate cannot reliably detect an older tag inserted behind an
unchanged head, historical deletion or metadata edits outside that head. These
need webhook invalidation or periodic reconciliation in a future phase; we do
not claim exhaustive history refresh. GitHub GraphQL has no 304 validator.

mise short names resolve through the published registry snapshot in
`crates/mise/registry.json` (`mise run registry:update`). Supported backends are
aqua/github/ubi/packslip (GitHub releases), npm, cargo, pypi, and supported core
tools. Unsupported plugins are not executed. Node uses its official distribution
index; Bun uses oven-sh/bun releases and strips bun-v tags; Go uses golang/go
tags and strips go; Python uses CPython tags; Rust uses rust-lang/rust releases.
Unknown tools report a warning. This resolves catalog identities, not custom
tool_alias settings, backend platform constraints, or plugin-defined version logic.
PyPI retains earliest file upload dates and all-files-yanked status; general PEP
440 policy remains outside this wave (mise uses loose numeric versions).

Hosted runner versions are maintained in `crates/datasource/src/runners.json`,
checked against [GitHub's runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
on 2026-10-15. Edit that data file and run CI to refresh support. Families are
separate for arm, Intel and Visual Studio variants; proposals never cross them.
The catalog tracks standard ubuntu, macos and windows labels. Custom larger
runners, xcode preview labels and ubuntu-slim are not version upgrade families.
The docs do not supply release timestamps, so runner versions have no age gate.

Public recordings use tiny-tarball, heck, rsc.io/quote, jdx/mr-boxington and
GitHub's hello-world action. Fresh registry responses reduce the warm source run
to three GitHub GETs, all 304; no .info or GraphQL history requests.

References: [npm metadata](https://github.com/npm/registry/blob/main/docs/responses/package-metadata.md),
[Cargo index](https://doc.rust-lang.org/cargo/reference/registry-index.html),
[Go proxy](https://go.dev/ref/mod#goproxy-protocol),
[GitHub GraphQL](https://docs.github.com/en/graphql/reference).
Additional specifications: [Node distribution index](https://nodejs.org/dist/index.json),
[PyPI JSON API](https://docs.pypi.org/api/json/),
[mise registry format](https://mise.jdx.dev/contributing/registry.html).
