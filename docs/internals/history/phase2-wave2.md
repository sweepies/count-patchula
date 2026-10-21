# Phase 2, wave 2

## Branch model

Non-major proposals (minor, patch and refresh of an existing digest) use one
`patchula/<package-slug>-<current-major>.x` branch. The best compatible proposal
per occurrence wins, avoiding overlapping edits. Major proposals retain only the
newest eligible major and use `<new-major>.x`. Zero majors retain `0.x`.
Eligible initial version/digest pins share `patchula/pin-dependencies`, including
pins otherwise assigned to a configuration group. Groups use `<group-slug>` and
`major-<group-slug>`. The configured `prs.branch` prefix replaces `patchula/`.

Lockfile-blocked pins retain separate skipped plans so they cannot block eligible
pins in the shared branch.

Package/source/line identities combine occurrences across files and container
managers. Packages with equal readable slugs gain consistent manager prefixes
across all lines (`docker-`, `mise-`, etc.).
A remaining ambiguity fails planning before any writes; explicit groups resolve
it. Container variants retain a suffix (e.g. `3.x-slim`) when several variants of
the same package occur; a singleton variant uses the major line. Nonnumeric
tracks use their literal slug (e.g. `latest`). Names contain no identity hashes. Ownership
continues to come from verified, bot-authored commit trailers and complete
inspectable history, never from a name alone.

Before applying current plans, a real run inventories all branches, validates
obsolete marked histories, and deletes obsolete owned heads with atomic head and
base leases. This includes wave 1 hash-suffixed branches even if their old
identity differs. Human, unsigned, unmarked and incomplete histories survive.
Dry runs compute only; cleanup discovery and write precondition reads run only
on real runs.

These names follow the owner's observed Renovate output. Deliberate departures:
multiple numeric container variants use the major plus variant, pins take
precedence over config groups, and a residual slug collision fails safely instead of adding
a hash. No Renovate source, presets or fixtures were consulted.

## Write performance

Wave 1 live evidence: `sandbox-run.err` reports write=75.475s, total=77.605s,
230 batch requests; `sandbox-dry.err` reports write=33.587s, total=36.790s,
170 batch requests. These are supplied measurements, not reruns by this agent.

The old writer issued four identical base precondition reads for every plan,
then a branch lookup, one blob POST per changed file, tree/commit POSTs and a
leased ref mutation. Dry runs performed all precondition and ownership reads.
The new dry writer sends **zero requests**. Real writes validate the base once,
run at most four independent branch tasks, and send only changed files inline in
one Git tree POST, followed by a commit POST. Prepared verified heads are
published in atomic batches of up to 49 update refs plus the base lease. For
all-active batches, new one-file branches therefore cost
`4 + 3*N + ceil(N/49)` requests instead of `9*N`, excluding
cleanup/PR lifecycle. Existing-branch signature/history verification remains.

Offline trace evidence (`shared_base_write_trace_and_request_budget`): twelve
branches, 150ms synthetic latency/request, 1s mutation cooldown: **41 requests,
28.500s simulated time**; dry run **0 requests, 0s simulated time**. The trace
prints method, host and path, verifies one base tree read and one atomic ref
publication, and rejects blob POSTs. This is a bounded deterministic API simulation, **not a measured
live speedup**. At 12 branches the prior request count was 108. The supplied
live timing cannot be directly compared to this simulation.

GitHub's [secondary limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api#about-secondary-rate-limits)
include 80 content requests/minute and 500/hour, and its
[best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api#pause-between-mutative-requests)
recommend serialized requests and a one-second pause after mutations. Client
clones now share a mutation lock, one-second post-response pause, rolling hourly
budget and primary/secondary cooldown. Reads remain bounded by the governor.
GraphQL queries do not consume the mutation budget. Mutation failures still
avoid blind retries except for explicit rate-limit refusals. Migration adds
one-time history inspection and ref deletion costs; PR creation adds its own
paced mutations. Do not promise a shorter first migration run than wave 1.

This session has no configured `PATCHULA_TOKEN`, `GITHUB_TOKEN` or `GH_TOKEN`.
Fresh live `-vv` before/after traces and write timings require the caller's App
installation token; no credential files were opened to recover one.

## SWE-107 pull requests

A real run inventories all PR pages, including closed history. Each update branch
gets one owned PR whose title is the policy-derived commit description. The body
contains a sorted package/change/type/file table, deduplicated `bodyNotes`, a
recheck checkbox, and a hidden JSON identity/target marker. Labels are combined
with existing labels, preserving manual labels. PR actions include numbers and
URLs in text and JSON reports. Repository errors include PR failures.

Manual closures suppress the same target set across base, file and title changes.
A new version/digest target can create a new PR. Existing open PRs are refreshed
without consuming creation capacity. `prs.concurrent` counts open owned PRs;
`prs.perHour` counts owned PRs created in the preceding hour, including closed or
merged PRs. Null means unlimited. Capacity is reserved before branch writes;
limits defer both new branches and PRs. Limits are repository-scoped within a
run, not a distributed lock against simultaneous independent CLI processes.

Superseded/satisfied owned PRs are closed before obsolete branch cleanup. Markers
alone do not authorize destructive changes: complete signed bot history is
validated, PRs refreshed immediately before closure, and deletions leased against
the observed branch and base. Any lookup warnings defer obsolete cleanup so a
registry failure cannot be mistaken for a satisfied update. Unowned histories,
unowned PRs and changed ownership markers are protected. Transport/5xx outcomes
are reconciled on the next run instead of blindly retrying creates.

Release notes now resolve ecosystem upstream metadata, select a version range
through the public mise-versions mirror and use GitHub releases with cached
changelog sections as a fallback. PR bodies retain a 32,000-byte budget, with
older versions collapsed and explicit truncation links. Unchanged bodies and
receipt-backed steady runs avoid note reads. See [release notes](/guide/release-notes)
for source mappings, cache identities, notification neutralisation, mirror
failure semantics and supported formats. `releaseNotes = false` skips both text
and requests.

`merge = true` for every update in a branch enables GitHub native auto-merge via
`enablePullRequestAutoMerge`, choosing an enabled squash/merge/rebase method in
that order, with an expected-head lease. Repository settings are included in the
shared inventory, so availability adds no steady-run request. Where native
auto-merge is off or unavailable, a later run can merge through GitHub's normal
merge endpoint after restoring the exact head and intent from a prior successful
write receipt. See [fallback merge gates and the sandbox rule](/guide/auto-merge).
Required checks and GitHub protections remain authoritative; no bypass or direct
base write is used. Mixed merge policy and lockfile-pending branches disable
existing native auto-merge and cannot use the fallback.

The checkbox says:

> Created by patchula CLI. Click to re-check on the next run.

`--recheck now|next-run` takes precedence over `PATCHULA_RECHECK`; `next-run` is
the default. Wording changes do not change checkbox identity: only its hidden
marker identifies a checked request. The next eligible run forces regeneration
and resets the checkbox after successful PR refresh. `now` changes wording for
external webhook runners; it does not add a server or scheduler to the CLI.

Offline lifecycle tests cover creation, update, unchanged repeats, checked retry,
manual closure across changed files/titles and paginated history, newer targets,
both limits/null, obsolete closure/deletion order, human-history protection,
lookup uncertainty, bounded/deduplicated/304 release notes, body-marker safety,
labels, native auto-merge, lockfile gating, reuse after a PR merge, and uncertain
create reconciliation.
CLI tests cover override precedence and invalid environment values.

## Expected sandbox convergence

Using the supplied `sandbox-run.json` catalogs and default sandbox config, the
next eligible run should create **16 branches and 16 PRs**, with `dependencies`
labels, no auto-merge, no default-branch changes, and no lockfile-bearing writes:

```text
patchula/actions-checkout-4.x
patchula/actions-checkout-7.x
patchula/docker-node-20.x
patchula/docker-node-26.x
patchula/jdx-mise-action-5.x
patchula/jqlang-jq-1.x
patchula/mise-node-22.x
patchula/mise-node-26.x
patchula/nginx-1.x
patchula/oven-sh-bun-1.x
patchula/pin-dependencies
patchula/postgres-16.x
patchula/postgres-18.x
patchula/redis-7.x
patchula/redis-8.x
patchula/ubuntu-26.x
```

The shared pin branch contains eligible action/image pins; prettier's blocked
version pin does not block it. Catalog changes, manual closures and config limits
can change this list; the new dry-run plan is authoritative for targets, and the
real inventory decides capacity/closed-target suppression.

Evidence discrepancy: the supplied JSON records **23** `created` actions and
11 skipped plans, including `jdx-mise-action-major` targeting v5.1.2. The brief
says 22 branches and a mise-action minor. Cleanup inventories actual heads and
retires **all owned obsolete hash branches**, whether the live count is 22 or 23.
No names or counts are hardcoded into cleanup.

## Commands

From the repository root, with the caller's App installation token exported as
`PATCHULA_TOKEN` (never place it in a command, checked-in file or trace):

```sh
CARGO_NET_OFFLINE=true mise run ci
CARGO_NET_OFFLINE=true mise exec -- cargo test -p patchula-update --test write shared_base_write_trace_and_request_budget --locked -- --nocapture
mise exec -- cargo build --release --locked -p patchula-cli
mise exec -- target/release/patchula -vv run --dry-run --json --cache sandbox.sqlite OWNER/SANDBOX > sandbox-wave2-dry.json 2> sandbox-wave2-dry.trace
mise exec -- target/release/patchula -vv run --json --cache sandbox.sqlite OWNER/SANDBOX > sandbox-wave2-run.json 2> sandbox-wave2-run.trace
# Repeat the real run to verify unchanged PRs/branches and warm note caching.
mise exec -- target/release/patchula -vv run --json --cache sandbox.sqlite OWNER/SANDBOX > sandbox-wave2-repeat.json 2> sandbox-wave2-repeat.trace
```

The App now needs Pull requests write and label-management permissions in addition
to Contents/workflow write. Inspect the `http` request lines per host plus the
`write`/`total` timing summary; request traces exclude tokens, headers, bodies and
query strings. A dry run still performs normal lookup reads; its write phase
performs zero requests. The first real run includes migration costs; repeat-run
and cold-create timings measure different workloads. The live commands above
were not run in this tokenless session.

Open follow-ups: collect live `-vv` traces with the caller's installation token;
verify PR/label permissions and native auto-merge on a repository with required
checks; add verified release-source metadata for non-GitHub catalogs before
expanding their release text. Keep migration ownership checks and rate pacing.


The final synthetic sandbox test replays the owner's target facts against
self-authored manifests and verifies the exact sixteen-name list above, five
eligible shared pins, and independent lockfile gating for prettier. The batch
publication test proves a failed lease publishes none of the prepared heads.

Existing branch ownership is inspected from the commit's recorded base rather
than today's default head, so a merged update branch remains reusable when the
next version on the same major line arrives. Signed marked bot work can follow
a changed track identity; copied markers never grant ownership to human commits.

## Final validation

`CARGO_NET_OFFLINE=true mise run ci` passed: 235 tests across 55 suites,
zero failures, one intentionally ignored recording-only test. The fixture stage
checked 22 cassettes; rustfmt, Clippy with `-D warnings`, tests and workspace
build passed. `mise run docs` regenerated the CLI reference and local generated
artifacts. Full CI log: `/tmp/patchula-wave2-ci-verified.log`; final stage logs:
`/tmp/patchula-ci.2GkmlT/`. Synthetic request trace:
`/tmp/patchula-wave2-final-trace.log`. No live sandbox write or push occurred.

CI tail (build stage):

```text
[build] $ cargo build --workspace --locked
   Compiling patchula-update v0.1.0 (crates/update)
   Compiling patchula-engine v0.1.0 (crates/engine)
   Compiling patchula-cli v0.1.0 (crates/cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.89s
```
