---
title: "Batching"
description: "Collect reviewed updates into one batch pull request by ticking a checkbox, then merge them together."
---
The batch PR lets a reviewer, human or agent, collect the updates it has judged
safe into one pull request, then merge that.

## Adding an update

Every update PR ends with an "add to the batch" checkbox, and every open
update PR on the [dashboard](/guide/dashboard/) has one too. Tick either box,
and on the next run (straight away under the [server](/guide/server/)) the
Count moves the update into the repository's batch PR on `patchula/batch` and
closes the original with a note in its body linking to the batch (no extra
comment is posted). Being batched never counts as being closed by hand, so the
update is not suppressed.

The batch keeps the version you reviewed. Each update PR records the targets it
showed, and the batch uses those, not whatever was released since. A newer
release opens as a fresh update PR once the batched one has closed; ticking that
one replaces the dependency's item in the batch.

Each PR is its own item. Groups that bump the same dependency to the same
version on separate branches, such as one group per service in a monorepo, add
one item each, and taking one out leaves the others in.

## How the batch is built

There is one batch per repository and base branch. It applies every item's edit
to the current base, and regenerates lockfiles once for the whole set, so a batch
of eight npm updates runs aube once, not eight times. Items whose targets the
base already meets, or has since moved past, drop out.

## Taking things out

Each item in the batch body has its own checkbox, in a collapsed list below the
release notes. Tick it and the update goes
back to its own pull request, on its original branch. If nothing is left, the
batch PR closes. Closing the batch PR without merging sends every item back.

## Merging

The batch never auto-merges unless you say so:

```pkl
prs { batchMerge = true }
```

Then it merges behind the same [check gates](/guide/auto-merge/) as any other
update.

## Skipping redundant CI

When every update in the batch came from a Count-owned PR that passed the same
clean-and-green check as [auto-merge](/guide/auto-merge/) on the exact base
commit the batch builds on, every item touched a disjoint set of files, the
batch commit writes the exact blob SHAs those source PRs tested, and no
lockfile is pending, the Count adds a trailer to the batch commit:

```text
Patchula-Batch-Green: true
```

The trailer is omitted if any item joined without clean green checks on that
base, if two items touch the same file (such as a shared `Cargo.lock` or
`mise.lock` regenerated together), if the batch commit differs from the tested
blobs, or once the batch rebases onto a new base. Note that under the default
`rebase = "conflicts"`, a batch without lockfile jobs stays on its old base
when the default branch moves, and source PRs must be green on the current base
tip to qualify.

On its own the trailer does nothing; a repository's workflow can inspect it in
a job-level `if:` so skipped jobs still report as `skipped` (satisfying
required status checks, unlike `[skip ci]`):

- Check that the PR comes from the same repository (not a fork), targets the
  batch branch, and was opened by your Count Patchula bot.
- Verify that the head commit has a single parent equal to the PR's base SHA,
  with `Patchula-Base` equal to that parent and `Patchula-Batch-Green: true`.
- If your repository enforces required workflows from protected refs, also
  verify the head commit's author login and GitHub signature
  (`verification.verified`) before trusting the trailer.

Two updates that pass on their own can still interact even when they touch
different files, so skipping batch CI is a workflow choice per repository.

## Details

The batch's items live in its body as a hidden marker, so any runner sees the
same batch without local state. The PRs it carries are listed under "Batched" on the [dashboard](/guide/dashboard/), and their branches stay until the
batch merges. A group whose targets would need more than 12,000 bytes of marker
cannot be batched. The `batch` branch name is reserved, so a group named
`batch` is a configuration error.
