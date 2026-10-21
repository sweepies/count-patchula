# Batch engine

`patchula-engine::Engine` accepts a token, shared SQLite cache, concurrency cap
and transport. The CLI constructs one network pool; tests inject cassette replay.
Its lookup method accepts repositories or discovers all accessible repositories,
using user/repos or the installation repository endpoint for App tokens.

Repositories run concurrently behind one GitHub governor. The default branch
name, commit and tree identity come from one GraphQL snapshot on a cold scan.
Only selected manager files and required local config modules are fetched as blobs. Truncated trees
and invalid selected files produce per-repo errors. The config is
patchula.pkl at the root; absence is skipped. Local --config
applies to every selected repository and is evaluated before network access.

Repository configs evaluate with an isolated supplied-file capability. Local
amends/import dependencies are fetched, with the bundled PatchulaConfig.pkl
available when not vendored. Remote and escaping imports/resources are
rejected. Literal imports are supported;
dynamic import targets require future discovery support. `config check` and
local overrides use the same supplied-module evaluator, and nested object
amendments retain class defaults in either path (SWE-105).
An unchanged commit/config fingerprint reuses extraction without tree/blob calls.
The extraction fingerprint includes the mise registry snapshot; its namespace
changes when extraction semantics change. Warm scans conditionally revalidate
metadata and the hinted branch in parallel; if the default branch changed, they
retry only the new branch. Both paths always check the current default branch.
Catalog and digest/ref requests are deduplicated across the whole batch. Opaque
action branches skip release history lookups, retaining their ref when pinned.
Warm source and branch-ref reads overlap repository revalidation using persisted
lookup hints. Hints only schedule work: the validated current extraction and
policy determine the report, and removed dependencies' speculative results are
discarded. Conditional tag namespaces already carry current and proposed tag
digests, avoiding a subsequent commit lookup. Opaque branches still revalidate
separately. `policy` measures CPU selection/report assembly;
`digest` measures any remaining ref wave. `extraction` includes the repository
metadata/head latency even on a warm extraction hit. `lookup` includes source prefetch
and remaining catalog work; it overlaps `extraction`, so phase times must not be
summed. Trace phase boundaries show that overlap. No Pkl evaluation,
tree/blob fetch or scanner work occurs on an unchanged cached repository config.

Report metadata includes branch/commit, warnings, source/kinds/locations, per-update
policy and request/cache counters plus phase timings. Unavailable upstream
identities produce warnings; API/config/extraction failures are errors. The CLI
writes only the report to stdout with --json and always summarizes on stderr.
It exits 1 for batch failures or any per-repo error, 2 for invalid config/options.
`lookup` remains read-only. `run` adds guarded branch planning/publication,
described below.

Validation includes a public two-repo replay using jdx/usage and jdx/mr-boxington,
plus independently simulated discovery/config-precedence/skips/errors tests.
Public fixture file selection and ignore exceptions bound recording size while
retaining all extracted dependencies. Source tests separately cover npm, Cargo,
Go, GitHub releases and tags, moving refs and incremental pagination.

```sh
mise run benchmarks:lookup
GH_TOKEN=$(mise exec -- gh auth token) mise run fixtures:record-wave1
mise run fixtures:check
mise run ci
```

The default benchmarks use debug engine runs against recorded responses, not a claim
about the twelve-repo production batch. Recording timings include network and
cassette redaction/persistence overhead. Persistent cache rows include repository
identities and selected manifest contents; keep private run caches/reports outside
this source repository.

Final wave-1 measurements (`mise run benchmarks:lookup`, debug, offline replay):
source fixtures: 54.11 ms cold / 28.15 ms warm (20 / 5 requests); two-repo batch:
227.42 ms cold / 42.56 ms warm (19 / 9 requests, three unique catalogs, two warm
extraction hits). Every warm network response is 304. The source fixture also
reuses two fresh validator-free Go responses without network access.
Public recording runs measured 3.29 s / 1.36 s for sources and 7.94 s / 4.64 s
for the batch, including recording overhead. These are individual observations,
not statistical benchmarks or live twelve-repo/App-token measurements.

Follow-up release replay on 2026-10-15 (single observations on this workspace):
baseline `2606e71`: 109.20 ms cold / 52.59 ms warm; implementation `3029efd`:
35.49 ms cold / 7.46 ms warm.
The same two public repositories are replayed, with npm responses refreshed from
abbreviated to full metadata to supply release dates. Warm requests fall from
9 to 8 (all 304); extraction hits remain 2 and unique catalogs remain 3. After
phases: extraction 3.17 ms, lookup 3.90 ms, CPU policy 0.37 ms. The historical
policy counter included digest HTTP waits, so those phase timings are not
directly comparable. Total time remains comparable. Reproduce with:

```sh
mise run benchmarks:lookup-release
# With an existing GitHub token, run twice against the same private cache:
mise run lookup:release --config examples/renovate-runner.pkl \
  --cache /tmp/patchula-runner.sqlite --concurrency 8 --json \
  sweepies/renovate-runner > /tmp/patchula-runner-report.json
```

The `3029efd` follow-up had no GitHub recording credential and refreshed only
public npm metadata. The subsequent live investigation refreshed both public
source and engine cassettes for GraphQL head batches and matching-ref hydration.
All ordinary tests remain offline. Current live results and reproduction commands
are in [benchmarks](/internals/history/benchmarks#live-network-investigation-2026-10-15).
Registry max-age is honored rather than replaced with an arbitrary local TTL.

## Branch write path

`Engine::run` shares lookup, discovery, configuration and policy with `lookup`.
The extraction cache now retains original file contents, Git blob SHAs, modes and
the full path inventory. Its v7 namespace invalidates older extraction rows.
Files needed only for lockfile detection need not be downloaded.

The pure updater derives branches and scalar edits from proposals and extracted
locations. npm aliases, Cargo inheritance, action refs/subpaths/comments, mise
forms, container tags and Go directive prefixes have explicit renderers. Edits
apply in descending byte order after overlap and UTF-8 boundary checks, preserving
all surrounding text. Uneditable generated Flux bundles, expanded Docker ARG
locations and missing separate YAML digest fields are reported as skipped rather
than rewritten incorrectly. Required but unavailable target digests also skip.

Before creating objects, the writer revalidates reads of the default head
and tree, comparing each extracted blob SHA and its actual content hash. A moved
base or stale blob refuses publication and requires a rerun. Git Data trees
include changed contents inline and create one replacement commit with the default commit as its
sole parent. No custom author, committer or signature is supplied; GitHub must
return `verification.verified = true` before a ref is published. See GitHub's
[bot signing requirements](https://docs.github.com/en/authentication/managing-commit-signature-verification/about-commit-signature-verification#signature-verification-for-bots).
The supplied wave-2 sandbox results confirmed verified App commits.

Existing branches require authenticated-actor authorship, matching bot/GitHub
committer, verified commits, and patchula's version/full-identity trailers on
every exclusive commit. Incomplete histories and human commits are protected.
Changes replace the branch commit in place; unchanged contents produce no writes.
`rebase = "behind"` recreates on a moved base. `"conflicts"` keeps an identical
manifest plan on its old base, recreating it when a touched file or proposal
changes. Actual branch blob hashes also verify purported unchanged plans.

Ref publication uses the Git Data GraphQL
[`updateRefs` mutation](https://docs.github.com/en/graphql/reference/git#updaterefs)
with `beforeOid` leases for both the branch and the default head. This is a
deliberate choice over REST's unconditional force update: it atomically refuses
concurrent human pushes and base movements. Object writes never enter the read
cache. Transport/server failures are not blindly retried; a later run reconciles
the current ref. Rate-limit refusals retain the existing governor behavior.

SWE-109 generates supported npm/Bun, pnpm, Yarn, Cargo, Go and Terraform
lockfiles with real tools through mise, in disposable trees backed by a
persistent partial clone.
Tool success and target validation precede API commits; failures are reported per
branch without publishing manifest-only work. Binary Bun locks
retain the explicit `--allow-lockfile-pending` testing escape hatch. Scheduled
lock maintenance is opt-in. See [wave-3 behavior and prerequisites](/internals/history/phase2-wave3).

Warm runs validate cached successful writes against a fresh GraphQL head/PR
inventory. Exact commit, update and PR matches bypass history/tree/PR reads and
tool execution. Changed inputs fall back to the full safety checks; tool-generated
branches also regenerate on base changes in conflicts mode. PR lifecycle,
closed-target suppression, labels and native auto-merge are documented in
[wave 2](/internals/history/phase2-wave2). Dashboard writes remain unimplemented.
