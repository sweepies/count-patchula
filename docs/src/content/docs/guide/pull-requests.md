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
read, not leased: a base that moves in between leaves the branch behind, and
the next run treats it like any other moved base, by your `rebase` setting
below. A branch carrying
commits that are not the Count's own verified commits is protected: he will not
rebase it, update it or delete it.

`rebase = "conflicts"` (the default) leaves a branch on an older base while it
still applies cleanly. `"behind"` rebases whenever the base moves. Branches with
regenerated lockfiles are rebuilt whenever the base changes, since a moved base
can change what the tool resolves.

## The PR

The title and commit message are
`<commitPrefix> update <dependency> to <version>`, or
`<commitPrefix> pin dependencies` for pin branches. `commitPrefix` is
`chore(deps):` by default and `fix(deps):` for an update that fixes an
advisory. The body opens with any
[tooling warnings](/guide/auto-merge/#tooling-warnings) for a PR set to merge
itself, then the advisories the branch fixes, if any, and for a pinned image's
new digest with what the Count found comparing its old and new SBOMs: a warning
when he can't tell whether the rebuild fixes anything, unless you silenced it
with `security.quietImages`, or a note when he couldn't read an SBOM on this run
(see [container image rebuilds](/guide/security/#container-image-rebuilds)). Then it
holds a table of every update on the branch (package, change, type, file),
the release notes for each package, folded into a collapsed block per
dependency (see [release notes](/guide/release-notes/#how-notes-are-rendered)),
any `bodyNotes` you configured, and two checkboxes:

- re-check, which forces the branch through verification and regeneration on
  the next run, or straight away under the [server](/guide/server/);
- add to the batch, which moves the update into the [batch PR](/guide/batch/).

The Count reads hidden markers, not visible text, so editing the wording does
not confuse him. Labels come from `labels`, `dependencies` by default.

### What the Count owns in the description

The Count owns the part of the description between his start marker
(`<!-- patchula-begin -->`, the first line he writes) and his ownership marker
(`<!-- patchula:… -->`, the last). The rest of the description is left alone: a
person's note above it, or a block that a summary or linking tool added or
rewrote in place, before or after. Whenever he writes the body he keeps that
text exactly as it is on GitHub at that moment. When nothing he renders has
changed he doesn't write the body at all, whatever else it holds, so repeated
runs leave it as it is.

If someone or something edits text *inside* his part, his next rewrite wins for
that part. Descriptions written before the start marker existed are his from
their first byte, once.

The body limit is GitHub's 65,536 characters. His part stays under 32,000
bytes, so about 30,000 bytes of other text fit beside it. Past 65,000 bytes in
all, the Count drops the text around his part rather than fail.

Advisory summaries, batch item titles and release-note prose are made inert:
mentions get a zero-width joiner, and links to GitHub issues go through
`redirect.github.com`. Package names in the table and your `bodyNotes` stay
exactly as they are.

## Closing a PR

Close an update PR without merging and the Count takes the hint: that target is
suppressed, and a new PR only appears when a newer version is available. Reopen
it to undo. The [dashboard](/guide/dashboard/) lists closed targets with a box to
recreate one.

When an update is no longer needed (merged elsewhere, dependency removed,
target superseded), its branch and PR are closed and deleted, with a lease on
the branch head so nothing unfamiliar is lost. Cleanup waits for the next run
if the default branch moved since it was read, and is skipped entirely in a run that reported
warnings, so a file that failed to parse never makes its updates look obsolete.

## Limits and holds

`concurrent` caps open PRs, at 10 by default, and `perHour` caps PRs created in
the last hour, with no cap by default. An update that is held waits for the
condition that holds it, and no other:

- `concurrent`: until an open PR merges or closes, which frees a slot at once;
- `perHour`: until the PRs created in the last hour age out. A PR still counts
  after it merges or closes, so merging or closing it frees no hourly capacity;
- a schedule window: until the next window opens;
- the release-age wait: until the release is old enough.

The [dashboard](/guide/dashboard/) shows each held update with its reason and a
box to create it now; without the dashboard the updates wait silently. A first
run on a repository with many outdated dependencies opens ten ordinary PRs and
lists the rest, and later runs open more as slots free up.

A security fix skips the `concurrent` and `perHour` checks: it is opened even
when ten PRs are open or the hourly cap is reached, so the total can pass 10.
Skipping the check does not take it out of the count. While it is open it is one
of the open PRs for `concurrent`, and for the hour after it is created it is one
of the PRs created in the last hour for `perHour`, even once merged or closed, so
it can leave less room for the ordinary updates that follow.
`security.apply` can still hold a fix by other means, such as `active`,
`waitHours` or `windows`. `concurrent = null` removes the cap.

The batch PR skips both checks too. It replaces the PRs it carries, which close
once it is open, so it never adds to the number of open PRs. Like a security
fix it is still counted: it is one of the PRs created in the last hour for
`perHour` for an hour after it opens, and the PRs it closes keep counting there
until their own hour is up.

## Repeat runs

After a successful write the Count stores a receipt: the commit and tree, the
inputs that produced them, and the PR's state. On the next run one GraphQL
inventory reads every branch head and open PR, and a matching receipt skips
the branch entirely. Change the base, the plan, the PR title, body or labels,
and that branch is reconciled again. Everything else stays skipped.
