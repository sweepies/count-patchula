---
title: "Batch engine"
---
`patchula-engine::Engine` takes a token, the shared SQLite cache, a
concurrency cap and a transport. The CLI builds one network pool; tests inject
cassette replay. A lookup takes named repositories or discovers every one the
token can reach, through `user/repos` or, for App tokens, the installation
repositories endpoint.

## Reading a repository

Repositories run concurrently behind one GitHub governor. On a cold scan, the
default branch, commit and tree come from one GraphQL snapshot, and only the
selected manager files and the config modules they need are fetched as blobs.
A truncated tree or an invalid selected file is an error for that repository.
A repository without `patchula.pkl` at its root is skipped. A local `--config`
applies to every selected repository and is evaluated before network access.

Configs evaluate in isolation over the supplied files. Local `amends` and
`import` targets are fetched, and the bundled `PatchulaConfig.pkl` is used
when the repository does not vendor it. Remote imports and imports that escape the
repository are rejected, and only literal import targets are supported.
`config check` and local overrides use the same evaluator.

An unchanged commit and config fingerprint reuses the previous extraction with
no tree or blob calls, Pkl evaluation or scanning; the fingerprint's namespace
changes whenever extraction semantics do. Warm scans revalidate the repository
metadata and the hinted branch in parallel, and always check the current
default branch. Catalog, digest and ref requests are deduplicated across the
batch. Persisted lookup hints let warm source reads overlap revalidation, but
hints only schedule work: the validated extraction and policy decide the
report.

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
a lookup failed, while `run` reports failed lookups and exits 0. The [CLI
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
```

:::caution
Cache rows include repository identities and manifest contents. Keep private
run caches and reports outside this repository.
:::

## Branch write path

`Engine::run` shares discovery, configuration, lookup and policy with
`lookup`. The extraction cache keeps each file's contents, Git blob SHA and
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
and `beforeOid` leases on the branch and the default head. Unlike REST's
unconditional force update, this atomically refuses a concurrent human push or
a moved base. Writes never enter the read cache, and transport or server
failures are not retried: the next run reconciles the ref.

**Lockfiles.** Lockfiles are regenerated with the real tools through mise, in
disposable trees backed by a persistent partial clone; see
[lockfiles](/guide/lockfiles/). The tool must succeed before any commit, and a
failure is reported for that branch without publishing manifest-only work.
`--allow-lockfile-pending` permits manifest-only test branches for binary Bun
locks.

**Warm writes.** Warm runs check [write receipts](/internals/cache/#write-receipts)
against a fresh GraphQL inventory of heads and PRs, and an exact match skips
history, tree and PR reads and tool runs. PR lifecycle, labels and auto-merge
are in [branches and PRs](/guide/pull-requests/) and
[auto-merge](/guide/auto-merge/).
