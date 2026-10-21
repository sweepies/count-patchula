# Native and fallback auto-merge

`merge = true` must hold for every member of the update branch. Pending lockfiles
and mixed policies cannot merge. The shared GraphQL inventory reads
`autoMergeAllowed`, enabled merge methods, repository ID and
`deleteBranchOnMerge`. It adds no settings-only request to a steady run.

When native auto-merge is allowed, patchula enables GitHub's native mechanism
using the enabled squash, merge, or rebase method, in that preference order.
The mutation includes `expectedHeadOid`; no status polling is added for native
auto-merge. Permission failures remain errors and are not silently converted
into permission to merge another way.

When native auto-merge is off or unavailable, the first run writes/reconciles the
PR and saves its verified receipt. Only a later run that restores that receipt
can merge. A missing receipt, changed intent/head/tree/author/PR body, invalid
signature, or unowned PR cannot enter the fallback. The ordinary writer's signed
history checks still protect branches with human commits. Restored candidates
must additionally satisfy all these gates:

- Current lookup evidence is complete, the effective policy still says merge,
  and every matching update occurrence passes release age and cron/timezone
  windows. Manual create, age and schedule permits do not waive merge policy.
- A fresh owned, non-draft, open PR still has the exact receipt head, base,
  repository, identity and body. `mergeable` must be true and
  `mergeable_state` must be `clean`. Conflicts, protection/review holds, behind,
  unknown and other non-clean states defer the merge.
- The current head's paginated GraphQL `statusCheckRollup.contexts` must contain
  at least one check run or commit status. Every check run must be `COMPLETED`
  with `SUCCESS`, `NEUTRAL` or `SKIPPED`; every commit status must be `SUCCESS`.
  Pending, failed, cancelled, timed-out, missing or unknown results block.
  Missing/partial responses, pagination errors and permission errors fail closed.
- PR ownership/head and clean mergeability are refreshed again after check
  inspection. `PUT /repos/{repo}/pulls/{number}/merge` includes the receipt's
  `sha` and an enabled `merge_method`. There is no admin/bypass option and no
  direct default-branch update. GitHub's merge endpoint still enforces current
  required checks and branch/ruleset protections.

GitHub's [check semantics](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks),
[merge-state definitions](https://docs.github.com/en/graphql/reference/pulls#mergestatestatus),
and [normal merge API](https://docs.github.com/en/rest/pulls/pulls#merge-a-pull-request)
are the public API evidence used here. The native availability fields are in the
[repository schema](https://docs.github.com/en/graphql/reference/repos#repository).
No Renovate implementation source was used.

After an acknowledged fallback merge, GitHub auto-deletes the head if configured.
Otherwise patchula atomically deletes only the exact receipt head with a lease
on the returned merged base SHA. A moved base/head or changed protection cannot
delete unfamiliar work: cleanup is deferred and included in the merged outcome.
Ordinary later-run obsolete-branch cleanup can retry it. Transport/5xx merge
errors are not blindly retried; the next inventory reconciles the actual state.

Reports use the additive `merged` action status and retain merge/cleanup reasons.
The dashboard displays check/policy waits beside open PRs and keeps up to
twenty fallback merge outcomes in actor-scoped local cache history. Native
steady runs retain the receipt/inventory fast path. Fallback check reads happen
only for still-open receipt-backed `merge = true` candidates; no checks are read
for newly created heads, disabled/mixed policies, policy holds or native mode.

## Sandbox configuration

Keep the existing `amends`, dashboard and zero-age sandbox settings. Append this
rule to `.github/patchula.pkl` (or add the rule inside its existing `rules` list):

```pkl
rules = new Listing {
  new Rule {
    select {
      managers = new Listing { "github-actions" }
      packages = new Listing { "jdx/mise-action" }
      updates = new Listing { "major" }
    }
    apply { merge = true }
  }
}
```

This targets only the mise-action major-update PR, avoiding the shared pin branch.
All other rules keep their normal merge policy. A complete, validated example is
[sandbox-automerge.pkl](../examples/sandbox-automerge.pkl).

Run the usual `mise run run -- sweepies/rustovate-sandbox` once to reconcile the
changed policy and persist its write receipt. Let that head's `ci / check` finish
successfully, then run the same command again. A head rewritten by rebase or
regeneration needs another later run with completed checks. In the private
sandbox with native auto-merge unavailable, the second eligible run should show
`merged` and the dashboard outcome. No successful checks means no merge. The
existing installation needs read access to checks/statuses plus the PR/contents
write permissions already needed for publication. Missing access is reported;
patchula never changes repository auto-merge settings or branch protections.

Offline engine tests cover successful check/status combinations, neutral/skipped,
no checks, pending/failure/cancellation, failure beyond the first page, drafts,
conflicts, protection/behind/unknown states, changed owner/human heads, head races,
API refusal, cleanup races, allowed method ordering, native inventory fast paths,
manual age/schedule permits, and persistent dashboard history. Live validation
is deliberately left to the sandbox owner; no live merges were performed.
