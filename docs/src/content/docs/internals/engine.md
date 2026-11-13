---
title: "Batch engine"
description: "How the engine reads repositories, looks up updates, builds the report and writes branches."
---
`patchula-engine::Engine` takes a token, the shared SQLite cache, a
concurrency cap and a transport. The CLI builds one network pool; tests inject
cassette replay. A lookup takes named repositories or discovers every one the
token can reach, through `user/repos` or, for App tokens, the installation
repositories endpoint.

## Reading a repository

Repositories run concurrently behind one GitHub governor. On a cold scan, the
default branch, commit and tree come from one GraphQL snapshot, and only the
selected manager files and the config modules they need are fetched as blobs,
in GraphQL batches (see [the GitHub client](/internals/github-client/#conditional-reads)).
A truncated tree is an error for that repository, as is a selected file
whose blob cannot be fetched or whose contents are not text — a
registry-settings file excepted, which stays unreadable so its packages
fail their lookups rather than using a public registry. A symlink is never
read through: the entry stays in the extraction as unreadable rather than
absent, and a symlinked settings file fails its packages' lookups the same
way. Files the include chain names in later rounds that cannot be read are
unreadable too. In a local checkout, files below a symlinked directory are
omitted from the tree entirely.
A repository without `patchula.pkl` at its root is skipped. A local `--config`
applies to every selected repository and is evaluated before network access.

Configs evaluate in isolation over the supplied files. Local `amends` and
`import` targets are fetched, and the bundled `PatchulaConfig.pkl` is used
when the repository does not vendor it. Remote and absolute imports, and
imports that escape the repository, are rejected, and only literal import
targets are supported. `config check` and local overrides use the same
evaluator.

An unchanged commit and config fingerprint reuses the previous extraction with
no tree or blob calls, Pkl evaluation or scanning; the fingerprint's namespace
carries this build's version, so a release that changes extraction semantics
never reuses the previous release's entries. A new commit reads the tree and the
selected files again, but those are cached by blob SHA, and config evaluation
and extraction are keyed by the content of the files they read as well: a push
that changes no file the scan reads costs one tree request and no Pkl
evaluation or scanning. The path inventory and unsupported-file list always
come from the current tree. Warm scans revalidate the repository
metadata and the hinted branch in parallel, and always check the current
default branch. Catalog, digest and ref requests are deduplicated across the
batch. Persisted lookup hints let warm source reads — current refs and the
mutable tags the last run resolved — overlap revalidation, but hints only
schedule work: the validated extraction and policy decide the report.

A complete run stores a `quiet-run` state row per repository and credential
scope: the head commit, a hash of the report row (tooling, lookup and
release-age evaluation timestamps, catalog evidence, stats and clock fields
excluded), `next_due` — the earliest instant time alone can change the
answer — and the run's timestamp. The server's quiet sweep check runs only
for batches the sweep alone claimed: candidates are rebuilt from the cached
config and extraction without a head check, the hinted digests are
prefetched, and the report is re-evaluated off the same catalogs. A
repository stays in the quiet set when its rebuilt report hashes the same,
its `next_due` has not passed and its last full run is under six hours old;
any missing state, failed reconstruction, passed due or hash mismatch runs
it in full — the check fails open toward running, never skipping.

## Report and exit codes

The report carries the branch and commit, warnings, sources, kinds and
locations, per-update policy, request and cache counters, and phase timings.
`extraction` includes repository metadata latency even on a warm hit, `lookup`
includes source prefetch and catalog work, `policy` is CPU selection and report
assembly, and `digest` is any remaining ref wave. `lookup` overlaps
`extraction`, so phase times must not be summed.

An unavailable upstream is a warning; API, config and extraction failures are
errors. The CLI writes the report to stdout, as text or with `--json` as
JSON, and `-v` adds the counters and phase timings on stderr. It exits 1 for a batch failure or any
per-repository error, and 2 for invalid config or options; `lookup` exits 4 when
a lookup failed, while `run` reports failed lookups and exits 0. A `run`
where a repository changed underneath it — a race the update layer reports
as `Error::Raced` — exits 5 instead, after an error has had its say. The [CLI
reference](/reference/cli/) lists every exit code.

## Tests and measurements

Recorded tests replay a public two-repository batch (jdx/usage and
jdx/mr-boxington) and the npm, Cargo, Go and GitHub sources. The benchmarks
replay those recordings, so they say nothing about a live batch.

```sh
mise run benchmarks:lookup
mise run benchmarks:lookup-release
GH_TOKEN=$(mise exec -- gh auth token) mise run fixtures:record-lookup
mise run fixtures:check
# A live batch: run twice against the same private cache.
mise run lookup:release --config examples/renovate-migration.pkl \
  --cache /tmp/patchula-runner --concurrency 8 --json \
  owner/repo > /tmp/patchula-runner-report.json
# Median phases of five warm `lookup -v` runs against live GitHub.
mise run benchmarks:live --cache /tmp/patchula-runner owner/repo
```

:::caution
Cache rows include repository identities and manifest contents. Keep private
run caches and reports outside this repository.
:::

## Branch write path

`Engine::run` shares discovery, configuration, lookup and policy with
`lookup`. The `extract` state keeps each file's contents, Git blob SHA and
mode, the full path inventory and the lockfile packages checked for advisories.

**Edits.** The updater derives branches and scalar edits from proposals and
extracted locations, with explicit renderers for forms such as npm aliases,
Cargo inheritance, action refs and comments, and Go directive prefixes. Edits
apply in descending byte order after overlap and UTF-8 boundary checks, so all
surrounding text is preserved. A location that cannot be edited safely, such
as a generated Flux bundle or an expanded Docker `ARG`, is reported as skipped.

**Commits.** Before creating objects, the writer re-reads the default head and
tree and checks each extracted blob SHA and content hash; a moved base or stale
blob refuses publication. One replacement commit has the default commit as its
only parent. No author, committer or signature is supplied, and GitHub must
return `verification.verified = true` before a ref is published (see
[bot signing](https://docs.github.com/en/authentication/managing-commit-signature-verification/about-commit-signature-verification#signature-verification-for-bots)).

**Existing branches.** Every commit on the branch that is not on the base must
be authored by the authenticated actor, committed by the bot or GitHub,
verified, and carry the Count's version and identity trailers. Otherwise the
branch is left alone. Changes replace the branch commit in place; unchanged
contents write nothing. `rebase = "behind"` recreates the branch when the base
moves. `"conflicts"` keeps an identical plan on its old base until a touched
file or proposal changes.

**Refs.** Refs are published with the GraphQL
[`updateRefs` mutation](https://docs.github.com/en/graphql/reference/git#updaterefs)
and a `beforeOid` lease on each branch. Unlike REST's unconditional force
update, this atomically refuses a concurrent human push. The default branch is
not leased: the writer checks the base head when it reads it, and a branch left
behind by a base that moves afterwards is handled by `prs.rebase` on the next
run. Cleanup rereads the default head just before deleting obsolete branches
and deletes nothing if it moved. The ref
update never names the default branch, because a no-op update of it is rejected
by rulesets that require pull requests. A rejected update reports GitHub's
GraphQL error messages. Writes never enter the read cache, and transport or server
failures are not retried: the next run reconciles the ref.

**Races.** Every step's outcome is one of three severities: an error, a
stale, or a warning. The update layer types its races as `Error::Raced` —
each names a state change observed mid-run, safe to rerun because the write
stopped before mutating against the stale view or its mutations are
idempotent on retry: the publish lease's base-head and metadata reads, the
write inventory, the obsolete-PR refresh and closure leases, the cleanup
metadata read, and the post-publish head read with its bounded replication
retries. A race leaves its branch or pull request action `stale`; the first
stale action lifts to the repository's `stale` status after errors and
protections have had their say, since an error always outranks a race. A
stale repository records no schedule turn, so the rerun still sees the
window the raced run covered; on the server it is requeued, never retried
with a backoff. Control and inventory reads use the same mapping.

**Lockfiles.** Lockfiles are regenerated with the real tools through mise, in
disposable trees backed by a persistent partial clone; see
[lockfiles](/guide/lockfiles/). The tool must succeed before any commit, and a
failure is reported for that branch without publishing manifest-only work.
`--allow-lockfile-pending` permits manifest-only test branches for binary Bun
locks.

**Warm writes.** Warm runs check [write receipts](/internals/cache/#write-receipts)
against a fresh GraphQL inventory of heads and PRs, and an exact match skips
history, tree and PR reads and tool runs. A full write reads only the pull
requests each decision needs: the open PRs, paged newest first; the PRs
created in the last hour when `prs.perHour` is set, the same listing walked
until a page ends older than the hour; and the closed PRs of exactly the
branches a planned update could publish on, batched into GraphQL
`pullRequests` reads paged per branch. A branch whose closed PRs cannot be
read leaves its plan protected rather than recreating a PR its owner closed.
PR lifecycle, labels and auto-merge
are in [branches and PRs](/guide/pull-requests/) and
[auto-merge](/guide/auto-merge/).
