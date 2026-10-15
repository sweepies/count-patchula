# Sources (SWE-97)

npm requests abbreviated metadata and retains dist-tags and deprecated flags.
crates.io reads the sparse index and excludes yanked releases during policy.
Go uses escaped module paths, @v/list, @latest and .info for new versions only.
Go proxy responses recorded here lack validators: their declared freshness is
honored, then aggregate list/latest requests refresh on expiry. npm and sparse
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

Public recordings use tiny-tarball, heck, rsc.io/quote, jdx/mr-boxington and
GitHub's hello-world action. The warm run performs only five network GETs, all
304, and uses two fresh Go responses; no .info or GraphQL history requests.

References: [npm metadata](https://github.com/npm/registry/blob/main/docs/responses/package-metadata.md),
[Cargo index](https://doc.rust-lang.org/cargo/reference/registry-index.html),
[Go proxy](https://go.dev/ref/mod#goproxy-protocol),
[GitHub GraphQL](https://docs.github.com/en/graphql/reference).
