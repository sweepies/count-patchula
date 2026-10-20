# Batch PR (SWE-155)

A reviewer, human or agent, can collect updates it considers safe into one pull
request. Every update PR ends with:

```text
- [ ] Add to the batch PR; this one closes. <!-- rustovate-batch-add -->
```

On the next run, or straight away in server mode, rustovate adds the update to
the repository's batch PR on `<prs.branch>batch` (default `rustovate/batch`). It
then closes the update PR with a comment linking to the batch. The closure
carries `<!-- rustovate-closed:batched -->`, so it never counts as a manually
closed update: a batched update is never suppressed.

- **Reviewed targets are kept.** Each update PR records the targets it showed
  in a hidden `rustovate-targets` marker, and the batch uses those, not the
  newest release. A newer release opens as a new update PR once the batched one
  has closed. Ticking that PR replaces the item for the same dependency.
- **One branch for everything.** There is one batch per repository and base
  branch. It applies every item's edit to the current base, and lockfiles are
  regenerated once for the whole set. Items whose targets the base already
  satisfies leave the batch.
- **Removing items.** Each item in the batch body has its own box. Ticking it
  returns the update to its own pull request. If no items remain, the batch PR
  closes.
- **Closing the batch.** Closing the batch PR without merging returns every item
  to its own pull request. Batch PRs never suppress updates either.
- **Merging.** The batch never auto-merges unless `prs.batchMerge = true`, which
  applies the usual auto-merge check gates.

The batch PR's items live in its body as a hidden `rustovate-batch` marker, so
any runner sees the same batch without local state. Values are JSON with `>`
escaped, so they cannot end the comment. The update PRs a batch carries are
listed under "Batched updates" on the dependency dashboard. Their branches stay
until the batch merges, so a removed item resumes on the same branch. A group
whose targets would exceed 12,000 bytes of marker cannot be batched.

The `<prs.branch>batch` name is reserved; a configured group named `batch` is
an error. In server mode, `pullRequestEdited` triggers on any newly ticked
rustovate control: re-check, add to batch or remove from batch.
