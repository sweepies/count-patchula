# Dependency dashboard (SWE-108)

`prs.dashboard = true` maintains one open issue titled **Dependency Dashboard**
initially. Discovery uses the exact `<!-- rustovate-dashboard:v1 -->` line and
the authenticated bot actor, so owners may rename it. Ambiguous owned dashboards
fail with a diagnostic rather than choosing one. Disabling the dashboard closes
the marked issue; enabling it again creates a new open issue.

The dashboard groups open PRs by update type, shows pending release-age targets
with eligibility times, PR budget and schedule holds, owner-closed targets,
dependency/source lookup warnings and stale provenance, and collapsible detected
dependencies by manager/file. Entries are sorted, escaped and bounded to GitHub's
body size. The receipt stores the rendered content hash; the fresh inventory
also verifies the actual body, so ticks and external edits cannot hide behind
the cached hash. Unchanged runs issue no dashboard mutation.

The shared GraphQL inventory includes open issue bodies, branch heads, open PR
bodies and closed PR receipt nodes. Both issue and ref pagination are followed
without treating a partial inventory as complete. Ordinary steady repositories
fit in one query. Issues from other authors and PRs without valid ownership
markers are excluded from the dashboard's ownership decisions.

Offline coverage uses synthetic GitHub transports, a checked-in rendering
snapshot, receipt restoration and an engine run with a reopened SQLite cache.
The warm engine test asserts one inventory query and zero mutations with the
dashboard enabled, for both open and owner-closed updates.

## Checkbox processing

Live runs read controls after extraction and before policy evaluation; lookup
and dry-run commands do not consume checkboxes. The same inventory is reused
for write verification. Dashboard actions carry a strict versioned hidden
action/target JSON marker. Dependency targets include manager, file, source,
package and current declaration/digest; update targets use the PR's existing
target identity. The repository re-check marker is bound to its repository.
Visible wording is never parsed. Foreign authors, invalid ownership markers,
duplicate actions and fenced examples do not authorize work.

- **Re-check this repository:** revalidate its dependency lookups and force
  regeneration/rebase of its existing owned PR branches. It preserves release
  age, budgets and owner closures for new updates.
- **Create this held-back/limited update now:** bypass release age for that
  exact dependency/version/class, or the selected update's PR budget/schedule
  hold. Other update policy, required digests, lockfile generation, signed
  ownership checks and atomic ref leases remain authoritative.
- **Recreate this owner-closed update:** create a new PR for the same target;
  leave the old PR closed. The explicit request bypasses its budget/schedule
  hold. An existing open replacement takes precedence over the old closure.
- **Retry failed lookup:** revalidate the dependency's source despite local
  HTTP freshness. Preserve validators, shared governors and upstream quota
  refusals. A failed attempt is unticked and remains visible in diagnostics.
- The existing PR **re-check** checkbox forces its own branch through write
  verification/regeneration and clears in the rendered PR body. Failed attempts
  are acknowledged with a body-only update, preserving fresh PR text.

Every acknowledged action is unticked or disappears when its pending row is
resolved. CLI wording says “on the next run”; `--recheck now` or
`RUSTOVATE_RECHECK=now` changes it to “now” without adding a webhook listener.

Accepted release-age targets are retained in the local cache for the same actor
and declaration, and in a hidden marker on the resulting bot PR. The shared
inventory recovers them after cache loss or dashboard disablement, preventing
later runs from reverting the selected version while its age window is closed.

## Sandbox exercise

Run `mise run run -- OWNER/SANDBOX` with the sandbox's existing token and config.
The first run creates/reconciles the marked dashboard; a second identical run
should make no GitHub mutation. Rename the issue and rerun to verify discovery
by marker. Tick each relevant row, save the issue/PR body and rerun the same
command. Verify the requested action and the cleared checkbox. Use a young
dated release, two competing updates with a PR limit of one, a closed schedule
window, and a manually closed rustovate PR to exercise the policy holds. A
failed/stale source lookup supplies its retry row. The offline suite covers
these scenarios without requiring live upstream failures.

Set `prs.dashboard = false` and rerun to close the owned issue. A human-authored
copy of a marked issue is ignored and left untouched. No live sandbox run is
part of the offline validation.

## Polish: targeted receipts and dashboard ordering

Checkboxes invalidate only matching update/branch receipts. Create and recreate
targets are resolved to plans before restoration (including release-age targets);
the invalidation applies to owner-closed receipts as well as successful writes.
PR re-checks invalidate their branch, and repository re-checks still invalidate
all plans. Lookup retries retain write receipts unless the resulting lookup
changes a plan's intent. Unmatched/stale targets are simply consumed. Untouched
plans retain receipt validation against the shared fresh inventory, including
owner closures, and skip individual branch verification and PR refreshes.

Offline engine coverage compares three and six dependencies for create,
recreate, PR re-check and stale retry clicks. Each create/recreate/PR click uses
16 repository requests, including extraction metadata/head reads, one shared
inventory, shared lifecycle/base reads, one target's writes and the dashboard
patch. Untouched PRs receive no individual fetches; adding dependencies adds
only their source lookups. A stale retry uses four repository requests and no
branch work. The following run makes no mutations in every case.

For the supplied nginx recreation trace, keeping the same lookup freshness and
repository state projects about **44 total requests / 20 write-phase requests**
instead of 143 / 119. Sixteen untouched open branches avoid five verification
requests and one PR refresh each, and three incidental release-note requests
disappear: 99 requests removed. The single inventory remains in extraction;
shared base/lifecycle/cleanup work and the target's ownership checks, atomic
lease, PR/labels and dashboard mutation remain. This is a projection from the
provided trace, not a new live run; source expiry, pagination, branch state and
release-note cache misses can change totals.

Dashboard sections now put open PRs first, then pending, owner-closed, warnings,
and finally collapsible dependency summaries. PR rows sort by numeric identity
within each section. The existing content hash and fresh-body comparison cause
an old layout to be patched once, then remain unchanged; the engine regression
also exercises this migration with a retained receipt and an owner-renamed title.

No new SWE-104 recurrence was present in the latest retained CI diagnostics
after `9804fde`; earlier development failures precede the latest passing wave 4
runs. No speculative flake fix was made.
