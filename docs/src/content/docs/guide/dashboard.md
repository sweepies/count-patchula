---
title: "Dependency dashboard"
---
The dashboard is on by default. To turn it off:

```pkl
prs { dashboard = false }
```

With the dashboard on, the Count keeps one issue per repository, titled
"Count Patchula's dependency dashboard" to begin with. Rename it if you like: he finds it by a
hidden marker and by who wrote it, not by title. Turning the dashboard off
closes the issue; turning it on again opens a new one.

## What it shows

In order:

1. known vulnerabilities, most severe first: one line per package and version
   that lists the first five files it appears in (and a count of the rest),
   with its advisories as links (and their summaries collapsed when there are
   several), and the fix once: the
   open PR that fixes it, "fixed in X, not proposed", or "no fixed release
   yet". A severity shows only when the advisory database rates it (see
   [security advisories](/guide/security/));
2. tooling warnings: updates set to merge in a repository whose CI, required
   checks or delivery don't back that up, with the open PRs each affects, and
   the ones you silenced with `warnings.quiet` collapsed under "Silenced" (see
   [tooling warnings](/guide/auto-merge/#tooling-warnings)); the section goes
   once the condition clears;
3. images the Count can't vouch for: pinned images whose SBOM can't show what
   a rebuild fixes, with the reason, and the ones you silenced with
   `security.quietImages` collapsed under "Silenced" (see
   [container image rebuilds](/guide/security/#container-image-rebuilds));
4. open update PRs, grouped by update type, then the ones he merged;
5. pending updates, each with the reason it is held (release age with the time
   it becomes eligible, such as "eligible 31 Oct 08:29 UTC", PR limits,
   schedule windows). A held update's lockfile or tool failure shows a short
   reason, with the decisive lines of the tool's output (from its first error,
   at most 10 lines) in a collapsed block. The full output stays in the CLI log
   and the JSON report;
6. template updates, which have no PR or checkbox
   ([Copier](/guide/managers/#copier)), and, under "Newer versions not proposed",
   releases whose declaration cannot be rewritten to allow them without
   guessing, each with the specific constraint that blocks it;
7. updates you closed ("Updates you closed"), with the target that was
   suppressed, and updates "Batched by the Count";
8. lookup warnings and sources served from stale cache: one box per failing
   package and source, listing every file that declares it. Ticking it
   retries the lookup for all of them;
9. every detected dependency, collapsed by manager and file.

Package names are code. Text that comes from outside the Count (tool output,
registry errors, advisory summaries, PR titles) is written so that GitHub
doesn't read a mention or an issue reference in it: an `@` or `#` followed by
a name or number gets an invisible zero-width joiner, and links to GitHub
issues and pull requests go through `redirect.github.com`. Nobody is
notified, and nothing is linked back, by what a tool printed. The same goes
for PR bodies.

The issue body is bounded to GitHub's size limit. An unchanged dashboard is not
rewritten: the Count compares the rendered content with the issue's actual
body, so ticks and edits are noticed even when his own records say nothing
changed.

## Checkboxes

Ticking a box asks for one thing:

- Re-check this repository: look everything up again and regenerate or
  rebase its branches. Release age, limits and your closures still apply to new
  updates.
- Create a held-back update now: skip the release-age wait, PR limit or
  schedule window for that one update. Everything else, digest pins, lockfile
  generation and ownership checks, still applies.
- Recreate a closed update: open a new PR for the same target and leave the old
  one closed.
- Retry a failed lookup: fetch the source again despite cached freshness.

Each box is unticked once its action is done, or disappears when its row is
resolved. Boxes are read on live runs only; `lookup` and `--dry-run` never
consume them. Box wording says when the click takes effect; see
[`--recheck`](/reference/cli/).

A box only counts when the Count's own hidden action marker is on that line and
the issue is his. Copies made by someone else, duplicate actions and examples in
code fences are ignored.

A release-age target you create early is remembered, in the cache and in a
marker on the resulting PR, so a later run does not revert it while its wait is
still running.
