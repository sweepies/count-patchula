---
title: "Branches and PRs"
---
Each update gets a branch, a single commit and a pull request. The Count owns
what he creates and leaves everything else alone.

## Branch names

Branches start with `prs.branch` (`patchula/` by default) and name the
dependency and the line it moves along:

| Update | Branch |
|---|---|
| a dependency on its 4.x line | `patchula/hono-4.x` |
| a container variant | `patchula/python-3.x-slim` |
| a group | `patchula/eslint`, and `patchula/major-eslint` for its majors |
| new pins and digests | `patchula/pin-dependencies` |
| lockfile maintenance | `patchula/lock-file-maintenance` |
| the batch | `patchula/batch` |

Registry-qualified packages use the package name, not the registry URL, so a
Helm chart from `https://stefanprodan.github.io/podinfo` gets
`patchula/podinfo-6.x`. When two managers produce the same name, the manager is
added to tell them apart.

## The commit

Every branch holds exactly one commit on top of its base, created through the
GitHub API so GitHub signs it. Updating a branch replaces that commit. Writes
use an atomic ref lease on the branch, so a concurrent push makes the write
fail instead of overwriting someone's work. The base is checked when it is
read, not leased: a branch left behind by a base that moves in between is
rebased on the next run. A branch carrying
commits that are not the Count's own verified commits is protected: he will not
rebase it, update it or delete it.

`rebase = "conflicts"` (the default) leaves a branch on an older base while it
still applies cleanly. `"behind"` rebases whenever the base moves. Branches with
regenerated lockfiles are rebuilt whenever the base changes, since a moved base
can change what the tool resolves.

## The PR

The title and commit message are
`<commitPrefix> update <dependency> to <version>`, or
`<commitPrefix> pin dependencies` for pin branches. The body opens with any
[tooling warnings](/guide/auto-merge/#tooling-warnings) for a PR set to merge
itself, then the advisories the branch fixes, if any, and for a pinned image's
new digest with what the Count found comparing its old and new SBOMs: a warning
when he can't tell whether the rebuild fixes anything, unless you silenced it
with `security.quietImages`, or a note when he couldn't read an SBOM on this run
(see [container image rebuilds](/guide/security/#container-image-rebuilds)). Then it
holds a table of every update on the branch (package, change, type, file),
release notes for each version in the range, any `bodyNotes` you configured,
and two checkboxes:

- re-check, which forces the branch through verification and regeneration on
  the next run, or straight away under the [server](/guide/server/);
- add to the batch, which moves the update into the [batch PR](/guide/batch/).

The Count reads hidden markers, not visible text, so editing the wording does
not confuse him. Labels come from `labels`, `dependencies` by default.

## Closing a PR

Close an update PR without merging and the Count takes the hint: that target is
suppressed, and a new PR only appears when a newer version is available. Reopen
it to undo. The [dashboard](/guide/dashboard/) lists closed targets with a box to
recreate one.

When an update is no longer needed (merged elsewhere, dependency removed,
target superseded), its branch and PR are closed and deleted, with a lease on
the branch head so nothing unfamiliar is lost. Cleanup is skipped entirely in a run that reported
warnings, so a file that failed to parse never makes its updates look obsolete.

## Limits and holds

`concurrent` caps open PRs and `perHour` caps new ones. Updates beyond a limit,
outside a schedule window, or still inside the release-age wait are held, and
the dashboard shows each with the reason and a box to create it now.

## Repeat runs

After a successful write the Count stores a receipt: the commit and tree, the
inputs that produced them, and the PR's state. On the next run one GraphQL
inventory reads every branch head and open PR, and a matching receipt skips
the branch entirely. Change the base, the plan, the PR title, body or labels,
and that branch is reconciled again. Everything else stays skipped.
