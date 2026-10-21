# Batching

Reviewing twenty dependency PRs one at a time is nobody's idea of a good
afternoon. The batch PR lets a reviewer, human or agent, collect the updates it
has judged safe into one pull request, then merge that.

## Adding an update

Every update PR ends with an "add to the batch" checkbox. Tick it, and on the
next run (straight away under the [server](/guide/server)) the Count moves the
update into the repository's batch PR on `patchula/batch` and closes the
original with a comment linking to the batch. Being batched never counts as
being closed by hand, so the update is not suppressed.

The batch keeps the version you reviewed. Each update PR records the targets it
showed, and the batch uses those, not whatever was released since. A newer
release opens as a fresh update PR once the batched one has closed; ticking that
one replaces the dependency's item in the batch.

## How the batch is built

There is one batch per repository and base branch. It applies every item's edit
to the current base, and regenerates lockfiles once for the whole set, so a batch
of eight npm updates runs aube once, not eight times. Items whose targets the
base already satisfies drop out.

## Taking things out

Each item in the batch body has its own checkbox. Tick it and the update goes
back to its own pull request, on its original branch. If nothing is left, the
batch PR closes. Closing the batch PR without merging sends every item back.

## Merging

The batch never auto-merges unless you say so:

```pkl
prs { batchMerge = true }
```

Then it merges behind the same [check gates](/guide/auto-merge) as any other
update.

## Details

The batch's items live in its body as a hidden marker, so any runner sees the
same batch without local state. The PRs it carries are listed under "Batched
by the Count" on the [dashboard](/guide/dashboard), and their branches stay until the
batch merges. A group whose targets would need more than 12,000 bytes of marker
cannot be batched. The `batch` branch name is reserved, so a group named
`batch` is a configuration error.
