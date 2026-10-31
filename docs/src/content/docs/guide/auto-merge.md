---
title: "Auto-merge"
---
Nothing merges unless a rule says so. The one rule built into the schema merges
[reference data](/guide/configuration/#reference-data), such as CA root bundles;
everything else needs a rule of your own:

```pkl
rules {
  new Rule {
    note = "Patch and digest updates merge once checks pass."
    select { updates { "patch"; "digest" } }
    apply { merge = true }
  }
}
```

Every update on a branch must have `merge = true`. A branch mixing merge and
no-merge updates, or one whose lockfile is still pending, never merges. The
batch PR has its own switch, `prs.batchMerge`.

## Native auto-merge

When the repository allows it, the Count turns on GitHub's own auto-merge for
the PR, using squash, merge or rebase (in that order of preference, among the
methods the repository allows), pinned to the exact head commit. GitHub then
merges once its required checks pass. A permission failure stays an error; it
is never taken as licence to merge some other way.

GitHub offers auto-merge only on a PR that cannot be merged yet. In a
repository where no required check or review stands in the way, a PR is
mergeable at once: its merge state is `CLEAN`, `UNSTABLE` (only non-required
checks are failing or still running) or `HAS_HOOKS`, and GitHub refuses with
"Pull request is in `<state>` status". That is not an error: the PR is treated
as if native auto-merge were off, and the Count merges it himself on a later
run, under all the conditions in the next section. That includes GitHub
reporting the PR `clean`, so an `UNSTABLE` or `HAS_HOOKS` PR waits. Any other
refusal, such as `dirty`, is still an error.

## When native auto-merge is off

The Count merges by himself, but only on a later run, and only when all of this
holds:

- The PR is one he wrote in an earlier run, with a stored receipt for exactly
  this head, base, body and author.
- Policy still says merge, and every update on the branch is past its release
  age. Creating an update early from the dashboard does not waive that.
  Schedule windows do not apply to merging.
- The PR is open, not a draft, and GitHub reports it mergeable with state
  `clean`. Conflicts, required reviews, a behind base or an unknown state wait.
- The head has at least one check run or commit status, every check run is
  completed with success, neutral or skipped, and every status is a success.

The [App needs read access](/guide/server/#create-the-github-app) to checks and commit statuses for this.

The dashboard shows open PRs that are waiting on checks or policy, and the last
twenty fallback merges.

## Tooling warnings

A PR set to merge itself carries a caution in its body, and the dashboard a
"Tooling warnings" section, when the repository shows it lacks what that merge
relies on:

| Warning | When |
|---|---|
| `merge-without-ci` | No workflow or CI file runs on the PR, and no check or status has reported on the default branch's last 10 commits. A `pull_request` workflow counts only when its `branches` include the default branch and its `paths` match a file the PR changes. Without native auto-merge, he waits for a passing check, so these PRs stay open; with it, nothing tests them before GitHub merges them. |
| `merge-without-required-checks` | The repository allows native auto-merge, and neither a ruleset nor classic branch protection requires a status check on the default branch, so GitHub merges a red PR as readily as a green one. The wording changes when a review is required instead. |
| `security-merge-without-delivery` | An update that fixes an advisory merges itself, and nothing runs when the default branch changes: no push workflow for it (directly, after one through `workflow_run`, or on a `schedule`), no Woodpecker push pipeline, no other CI file, no Flux or Argo CD manifests, and no checks on recent commits. A push workflow whose `paths` skip every file the PR changes counts as nothing. When a workflow runs on tags or releases, the warning says that is the only thing here that could deliver the fix, and nothing is known to publish or deploy it. |

A warning needs positive evidence. A workflow the Count can't parse, or branch
rules he can't read, means he doesn't know, and he says nothing. To find Flux
and Argo CD manifests he reads every YAML file in the repository, but only
when something can merge on its own and the warning isn't silenced; with more
than 300 that no manager reads, or if one can't be fetched, delivery is unknown
and `security-merge-without-delivery` stays quiet. Other CI files
are `.gitlab-ci.yml`, `.circleci/config.yml`, `Jenkinsfile`, `.buildkite/`,
`azure-pipelines.yml` and `.travis.yml`, taken to run on every change.

Silence a warning with its reason, which is required:

```pkl
warnings {
  quiet {
    ["security-merge-without-delivery"] = "Argo CD in ops/ syncs main"
  }
}
```

A silenced warning stays off PRs; the dashboard lists it, with your reason,
under "Silenced" while the condition holds. The checks need no extra App
permission: the evidence comes from the tree the Count already reads and from
fields on his existing inventory query.
