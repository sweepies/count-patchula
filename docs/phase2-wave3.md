# Phase 2, wave 3

## Fast repeats

The supplied wave 2 release trace measured 24.4s total / 21.1s write on an
unchanged repository, with per-branch tree, commit, history, PR and viewer reads.
Successful writes now store a receipt containing the verified commit/tree OIDs,
input identity, final file diffs and canonical PR state. A fresh paginated
GraphQL inventory reads heads, commit metadata and associated open PRs, including
title, body, author, head/base, labels and native auto-merge state. The inventory
also checks the default head and detects obsolete marked heads outside the
current prefix. All heads are inventoried so prefix changes and legacy names
cannot hide obsolete work.

A matching receipt skips branch verification, tool execution and PR
reconciliation. A mixed run also skips matching branches individually. Changed
heads, plans, policies, PR edits/closure, missing PRs, ambiguous PRs, truncated
connections and obsolete heads fall back to ordinary reconciliation or fail
closed. Behind mode invalidates on base changes. Conflicts mode retains an
unchanged manifest on an older base; tool-generated branches regenerate whenever
the base changes so changed lockfiles or source imports cannot be overlooked.
Copied trailers never establish a receipt. Only verified writes or complete
ownership/tree validation can establish one. Remaining REST reads revalidate
cached validators; writes still use atomic branch/base leases.

Offline evidence: `verified_receipts_reduce_warm_write_to_one_inventory_and_detect_changes`
uses **one query, zero mutations, 200ms simulated elapsed** with 200ms request
latency. It covers head, body, title, labels, merge state, closure, base and plan
changes, obsolete heads and pagination. This is synthetic evidence, not a live
speed claim. The first upgraded run must establish receipts. No installation
token is configured in this implementation session, so no live `-vv` run was made.

## Wave 3 live fixes

The supplied steady trace measured 22.146s total / 19.943s write and 134 network
requests. The cache was inspected read-only: all 16 open-PR receipts existed
under the branch-prefix key with the current base. Their PR `head`/`base` fields
contained the full REST objects (including repository metadata), while the
inventory reconstructed only ref/SHA/repository identity. Those objects could
never compare equal. The synthetic server had returned minimal REST objects,
masking the mismatch. Receipt state now explicitly selects the same fields from
both APIs; tests include extra REST fields.

Seven manually closed targets also remained planned until full PR-history
inventory. Version 3 receipts retain their verified closed PR IDs, target intent
and body. The same inventory query checks those nodes for closed/unmerged state,
author, repository and unchanged marker/body. Reopening, merging, deletion,
ownership/body edits or a new target invalidates the proof. The first upgraded
run performs full verification and seeds v3 receipts, including existing branches
that were not created in this run. No cache migration or live cache write was
performed during implementation.

Inventory cleanup detection excludes the default branch and tips whose
author/committer/signature already rule out ownership. A foreign actor's branch
does not force repeated verification; plausible obsolete owned tips still fall
back to full history inspection and leased cleanup.

After publishing a head, PR reconciliation accepts the published commit or polls
the exact previously verified head. Six waits of 100/200/400/800/1600/3200ms allow
seven fresh reads. An unfamiliar head fails immediately; exhaustion reports
propagation timeout rather than claiming the head moved. Every read rechecks
ownership and closure. An owner closure during publication or obsolete-PR
inspection produces `skipped` with a `closed-by-owner` reason and preserves the
PR number. It does not fail the repository. Policy/limit/closed-target skips are
non-errors; transport/API failures, unexpected heads, exhausted propagation,
ownership changes and protected histories still fail the repository.

Offline measurement: the complete engine creates once, reopens its persistent
SQLite cache in a fresh engine, then makes exactly **four** requests: repository
metadata, default-branch head, npm lookup and one inventory query. Both open and
closed cases take **600ms simulated total at 200ms/request**, with metadata/head
reads overlapping lookup. The write inventory alone takes **200ms**, zero
mutations. Tests also cover receipt adoption, foreign obsolete tips, pagination,
actual rebases, propagation timeout and closure while polling. These are
deterministic offline measurements, not new live timings.

For the supplied sandbox state after v3 receipts are seeded, an unchanged run
needs exactly **one write-phase POST /graphql query** (one refs page plus the
seven closed PR nodes), with no per-branch ref/commit/tree/compare/pull requests,
PR-history/branch-list/viewer requests, release-note reads or mutations.
The supplied lookup phase made 24 network requests and 22 fresh cache hits:
one repository-head GraphQL query, five GitHub release-list GETs, three GitHub
tag-ref GETs, one Node catalog GET, one Cargo sparse GET and thirteen Docker
manifest HEADs. With the same freshness and credential conditions, that projects
to **25 total network requests**, ten to GitHub. A reusable credential-scoped
branch hint replaces the one repository-head query with two conditional REST
reads (repository metadata and `branches/main`), giving **26 total**, eleven to
GitHub. Expired catalogs, changed dependencies/base/PRs, additional refs pages,
authentication retries or another actor's concurrent edits change those counts.

## Pin wording

Pin-only commits and titles use `<commitPrefix> pin dependencies`. Pin rows stay
in the table, while both release-note requests and unavailable-note notices are
omitted for those rows. Mixed updates can still show notes for their version
updates. A wording change refreshes the branch/PR without changing closed-target
suppression.

## SWE-109

The cache's sibling `<cache-stem>.worktrees` directory holds one bare partial
clone per repository. Initialization requests `blob:none`; only a changed base
needs an explicit shallow fetch. A repository lock protects shared clone operations.
Each branch gets a separate temporary tree and Git index. Only manifests,
lockfiles, mise version files, Rust/Go source needed for resolution, and other
files explicitly edited by the plan are checked out. Blob hashes must agree
with Git and the API extraction before edits are applied. Symlinks, special
files, unexpected generated changes, oversized files and non-UTF-8 output fail
the branch. The source tree is never pushed or committed with Git.

Tools run through `mise --no-config exec` with an explicit tool version. Only
version strings or version-table entries for relevant tools are read from
ancestor repository `mise.toml` / `.mise.toml` files. Repository environment
blocks, hooks and unrelated tools are not loaded. Defaults live in
`crates/update/src/lockfiles/defaults.toml`: node 24.21.0, Bun 1.4.3, Rust 1.99.0,
Go 1.27.2 and Terraform 1.14.7. Rust uses its pinned rustup toolchain behind mise
while keeping Cargo's configuration and credentials home isolated. Go is also
pinned in this repository's mise config for offline fixture tests.

The commands are:

```text
bun install --cwd <root> --lockfile-only --ignore-scripts
npm install --prefix <root> --package-lock-only --ignore-scripts --no-audit --no-fund
cargo update --manifest-path <root>/Cargo.toml -p <crate>@<old> --precise <target>
go -C <root> get <module>@<target>
go -C <root> mod tidy       # lockfiles.tidyGo=true
go -C <root> mod download   # otherwise
terraform -chdir=<root> providers lock
```

Cargo toolchain-only changes use `cargo update --workspace`; maintenance uses
untargeted `cargo update`. Both npm and Bun locks are regenerated when both are
present. Maintenance uses npm `update --package-lock-only` and regenerates the
Bun lock. Terraform regenerates provider selections from the manifest
constraints without initializing or accessing a backend. Missing module
initialization, unavailable providers or other tool errors are branch failures.

Every process starts from an environment allowlist with an isolated HOME, npm
config, Cargo home and Go paths. Git alone receives a host-scoped temporary HTTP
Basic header in its process environment for installation-token access. It is
absent from remote URLs, argv, stored Git config and every ecosystem process.
Git errors redact the header, encoded credentials and token. Parent tokens,
cloud credentials, Git helpers and tool configuration are not inherited.
Each command has a 180s deadline; timeout kills the process group on Unix.
Output is drained with bounded buffers and retained in diagnostics only on
failure (at most 4KiB stdout plus 4KiB stderr). Successful scratch trees are
removed, as are failed scratch trees. Only the partial clone and tool cache
remain. No API commit/ref is created until every tool for that branch succeeds
and the selected manifest/lock resolution still matches its requested targets.
Lock additions and deletions join the manifest in the same verified API commit.

Text Bun locks, npm package locks/shrinkwrap, Cargo, Go and Terraform are covered.
npm and pnpm locks are planned to move to aube by default; see
[package managers and speed](../README.md#package-managers-and-speed).
Yarn, pnpm and binary `bun.lockb` remain unsupported. The retained
`--allow-lockfile-pending` flag is an explicit manifest-only testing escape hatch
for those formats; supported formats always require successful tooling.

`lockfiles.refresh=true` maintains `<prs.branch>lock-file-maintenance` (default
`rustovate/lock-file-maintenance`). It groups all enabled ecosystems, uses default
labels/merge policy, and reports the configured lockfile group and changed
files. Maintenance runs at most once per ISO week, respecting `waitHours` since
the last successful completion. The default eligible window is Monday
00:00–04:00 UTC; explicit default update windows replace it. The CLI must be
invoked during the window; it does not install a scheduler. The desired branch
remains reserved between windows so an outstanding PR is preserved. Failures do
not consume the successful-maintenance checkpoint. Closed maintenance updates
can retry in a later week.

Offline tests use local path packages, a local vendored Cargo registry, a local
Git remote and disabled tool auto-installation. They exercise actual npm, Bun,
Cargo and Go commands, deterministic output, targeted Cargo/Go updates, clone
reuse/incremental fetch, environment scrubbing, tool failure without API writes,
output bounds, timeouts, weekly maintenance and API additions/deletions. Terraform
command construction/failure boundaries are covered without contacting a
provider registry; live registry/signature behavior remains a sandbox check.

## Sandbox expectations and prerequisites

The supplied report pins Bun **1.1.30** in `mise.toml`, predating text `bun.lock`
support. Repository pins are honored: generation reports an actionable branch
error rather than silently choosing a newer tool. Recommended prerequisite:
merge the existing `rustovate/oven-sh-bun-1.x` PR targeting Bun 1.4.3, then rerun.
This also satisfies the default-tool policy without adding an override.

With compatible tools and unchanged catalogs/config, seven additional dependency
branches and PRs are eligible:

```text
rustovate/github-com-google-uuid-1.x
rustovate/go-1.x
rustovate/hono-4.x
rustovate/typescript-5.x
rustovate/typescript-7.x
rustovate/zod-3.x
rustovate/zod-4.x
```

Prettier joins the existing pin branch, bringing it to six table rows; it does
not get a separate PR. If the Bun update is merged first, that resolved PR/head
leaves the active set: the supplied 16-open baseline would usually become 22 open
PRs after seven new ones, before other merged updates, limits or catalog changes.
Without the Bun prerequisite, only the two Go branches can proceed; Bun-bearing
plans, including the combined pin branch, fail without moving existing heads.

Cargo has **no update proposals in the supplied report**: anyhow/serde resolve
to current eligible versions. Do not expect invented Cargo PRs. A newer eligible
target will use its normal dependency branch with a generated Cargo.lock;
scheduled refresh can independently maintain it when enabled. Terraform likewise
needs an eligible provider plan or enabled maintenance.

## Commands

From `/root/rustovate`, with an installation token supplied through the existing
environment variable (never written in commands or files):

```sh
CARGO_NET_OFFLINE=true mise run ci
CARGO_NET_OFFLINE=true mise exec -- cargo test --locked -p rustovate-update --test write verified_receipts -- --nocapture
mise exec -- cargo build --release --locked -p rustovate-cli
mise exec -- target/release/rustovate -vv run --dry-run --json --cache /root/rustovate-conf/sandbox.sqlite sweepies/rustovate-sandbox > /root/rustovate-conf/sandbox-wave3-dry.json 2> /root/rustovate-conf/sandbox-wave3-dry.trace
mise exec -- target/release/rustovate -vv run --json --cache /root/rustovate-conf/sandbox.sqlite sweepies/rustovate-sandbox > /root/rustovate-conf/sandbox-wave3-run.json 2> /root/rustovate-conf/sandbox-wave3-run.trace
mise exec -- target/release/rustovate -vv run --json --cache /root/rustovate-conf/sandbox.sqlite sweepies/rustovate-sandbox > /root/rustovate-conf/sandbox-wave3-repeat.json 2> /root/rustovate-conf/sandbox-wave3-repeat.trace
```

Compare repeat `write`/`total` timing and request traces. Expect one write-phase
inventory query per page and no REST reads or mutations when every planned
branch/PR matches a receipt. Ordinary lookup conditional requests remain.
The dry run performs no tool execution or write-phase requests; generated
lockfile diffs are only known after a real run.

Open decisions: merge the existing Bun update before the live sweep (recommended)
or explicitly design a minimum-tool override; retain the Monday maintenance
default or choose explicit update windows; inspect Terraform provider locking in
a live sandbox before broad rollout. Private registries/modules will fail with
the scrubbed environment: scoped registry credential injection should be a
separately designed opt-in, not inherited process secrets.

## Validation

`CARGO_NET_OFFLINE=true mise run ci` passed: 247 tests across 55 suites,
zero failures and one intentionally ignored recording test. The fixture scan
checked 22 cassettes; formatting, Clippy with `-D warnings`, tests and the workspace
build passed. Full log: `/tmp/rustovate-wave3-ci.log`; stage logs:
`/tmp/rustovate-ci.f5imma/`. `mise run docs` regenerated the CLI reference.
The release CLI also built successfully; its log is
`/tmp/rustovate-wave3-release.log` (the linker emitted its deprecated optimization
setting diagnostic). No live sandbox mutation or push was performed.

CI tail:

```text
[build] $ cargo build --workspace --locked
   Compiling rustovate-update v0.1.0 (/root/rustovate/crates/update)
   Compiling rustovate-testkit v0.1.0 (/root/rustovate/crates/testkit)
   Compiling rustovate-engine v0.1.0 (/root/rustovate/crates/engine)
   Compiling rustovate-cli v0.1.0 (/root/rustovate/crates/cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.00s
```
