# Auto-merge

Nothing merges unless a rule says so:

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

## When native auto-merge is off

The Count merges by himself, but only on a later run, and only when all of this
holds:

- The PR is one he wrote in an earlier run, with a stored receipt for exactly
  this head, base, body and author.
- Policy still says merge, and every update on the branch is past its release
  age and inside its schedule window. Creating an update early from the
  dashboard does not waive these.
- The PR is open, not a draft, and GitHub reports it mergeable with state
  `clean`. Conflicts, required reviews, a behind base or an unknown state wait.
- The head has at least one check run or commit status, every check run is
  completed with success, neutral or skipped, and every status is a success.
  Pending, failed, cancelled, missing or unreadable results block the merge.

Ownership and mergeability are read again after the checks. The merge call sends
the receipt's head SHA, and GitHub still enforces branch protection and rulesets.
There is no admin bypass and no direct push to the default branch. After a merge
the head branch is deleted, unless GitHub already did it, with a lease so a moved
branch is left alone.

The App needs read access to checks and commit statuses for this. The Count
never changes a repository's auto-merge setting or its branch protection.

The dashboard shows open PRs that are waiting on checks or policy, and the last
twenty fallback merges.
