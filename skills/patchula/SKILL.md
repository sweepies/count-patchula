---
name: patchula
description: Configure and run Count Patchula, the dependency update bot for GitHub. Use when working with the patchula CLI, a patchula.pkl config, or patchula's JSON reports and pull requests.
---

# Count Patchula

Count Patchula reads a repository's manifests, looks up newer versions, and
opens one pull request per update with the lockfile regenerated. The CLI is
`patchula`. Full documentation: https://count-patchula.sweepy.dev, indexed for
agents at https://count-patchula.sweepy.dev/llms.txt. Every page is served as
Markdown to a request with `Accept: text/markdown`.

## Configure a repository

A repository opts in with a `patchula.pkl` at its root. Without one, the
Count skips it. Configs are
[Pkl](https://pkl-lang.org) amending the published schema:

```pkl
amends "PatchulaConfig.pkl"

rules {
  new Rule {
    select { updates { "patch" } }
    apply { merge = true }
  }
}
```

Validate every change before committing it. `config check` exits 2 on an
invalid config and names the file and line:

```sh
patchula config check .
```

The Count supplies `PatchulaConfig.pkl` when the repository has no copy.
`patchula config schema > PatchulaConfig.pkl` vendors it for editor
completion; a vendored copy is the one evaluated.

Unknown fields are errors, so read the schema rather than guessing a field:
https://count-patchula.sweepy.dev/reference/configuration/. Rules, selectors
and what a rule can change are in
https://count-patchula.sweepy.dev/guide/configuration/.

## Run it

Export a GitHub token in `PATCHULA_TOKEN` (`GITHUB_TOKEN` and `GH_TOKEN` also
work); real runs need a GitHub App installation token. Read before writing:

```sh
patchula lookup --json > report.json   # dependencies and proposed updates
patchula run --dry-run                 # branches, files and diffs for this repo on GitHub
patchula run                           # create branches and pull requests on GitHub
patchula remote run                    # run on the Count server instead of locally
```

With no repository argument, `lookup`, `apply` and `tui` read the working tree
in the current directory, uncommitted changes included, and `tui` can apply
selected updates to it. `run` always reads GitHub (even with `--dry-run`), using
this checkout's GitHub remote when no repository is named; pass `--dry-run --dir .`
to plan branches from a local directory. Name repositories as `owner/repo`, or
pass `--all-repos` for every repository the token can reach; both read through
the GitHub API. `patchula remote` (`lookup`, `run`, `tui`, `login`, `logout`)
runs on a Count server (`--server`, `PATCHULA_SERVER`, or the server advertised
by the repository) while top-level commands always run on this machine.

`patchula tui` browses the same results interactively. `patchula apply`
writes the working tree's updates into its files without PRs; filter with
`--manager`, `--dependency` and `--type`, preview with `--dry-run`, and use
`--check` in hooks and CI (exit 3 while updates wait). See
https://count-patchula.sweepy.dev/guide/apply/.

## Read the results

`--json` prints one report for the batch. Check each repository's `status`:
`skipped` and `error` carry a `reason`, and an `error` makes the CLI exit 1.
Field meanings are in https://count-patchula.sweepy.dev/reference/report/.

An update that is missing usually has a reason in policy: the 24-hour release
age wait (`metadata.heldBack`), a rule with `active = false`, or a `permitted`
filter. See https://count-patchula.sweepy.dev/guide/lookup-policy/.

## Pull requests

Change what the Count proposes through the config. A `patchula/` branch that
gains a commit he did not make is protected: he stops rebasing, updating or
deleting it. PR checkboxes re-check an update or move it into a batch PR; see
https://count-patchula.sweepy.dev/guide/pull-requests/ and
https://count-patchula.sweepy.dev/guide/batch/.
