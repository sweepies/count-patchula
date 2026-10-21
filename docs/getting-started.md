# Getting started

This page takes one repository from nothing to its first update pull requests.
You need a GitHub token, [mise](https://mise.jdx.dev) on the machine that runs
the Count, and about five minutes.

## 1. Install patchula

```sh
cargo install --git https://github.com/sweepies/count-patchula --locked patchula-cli
patchula --version
```

There are no release binaries yet. See [installation](/installation) for
building from a checkout and for what the runner needs at run time.

## 2. Add a config to the repository

The Count only visits repositories that invite him. Commit a
`.github/patchula.pkl` (or `patchula.pkl` at the root):

```pkl
// .github/patchula.pkl
amends "../Patchula.pkl"

prs { dashboard = true }
```

That is a complete config. Everything else comes from the schema's defaults:
every supported manager, exact version pins, digest pins for actions and
images, a 24-hour release-age wait, one PR per update and no auto-merge.

`Patchula.pkl` does not need to exist in the repository. When the Count reads a
repository's config he supplies the schema himself. Vendor it if you want to
check the config locally or get completion in an editor:

```sh
patchula config schema > Patchula.pkl
patchula config check .
```

## 3. Get a token

Export a token in `PATCHULA_TOKEN` (`GITHUB_TOKEN` and `GH_TOKEN` also work).

- For `lookup` and dry runs, any token that can read the repository is enough.
- For real runs, use a GitHub App installation token. Commits are created
  through the API so GitHub signs them, and the Count checks that branches he
  touches only contain his own verified commits. The App needs contents, pull
  requests, issues and workflows write access, plus read access to checks and
  commit statuses.

Long-running setups can let the Count mint installation tokens himself. See
the [GitHub App server](/guide/server).

## 4. Look before you leap

```sh
patchula lookup --json sweepies/my-app > report.json
patchula run --dry-run sweepies/my-app
```

`lookup` prints a summary and writes the [JSON report](/reference/report): every
dependency found, its current version and the proposed updates. `run --dry-run`
goes further and prints the branches, files and diffs it would publish,
without touching GitHub.

## 5. Run it

```sh
patchula run sweepies/my-app
```

The first run opens one PR per update and, with `dashboard = true`, a
Dependency Dashboard issue. Lockfiles are regenerated with the real tools,
which mise installs on first use into a tool store kept beside the cache. Run
the same command again and the Count notices nothing changed: an unchanged
repository costs one GraphQL query on the write side.

Omit the repository argument to process every repository the token can reach.

## Where to go next

- [Configuration](/guide/configuration) for rules, groups, schedules and limits.
- [Branches and PRs](/guide/pull-requests) for what the Count writes and the
  checkboxes he reads.
- [Coming from Renovate](/renovate/compatibility) if you are switching.
- [GitHub App server](/guide/server) to run him continuously from webhooks.
