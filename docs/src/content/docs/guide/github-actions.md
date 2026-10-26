---
title: "GitHub Actions"
---
The repository is also a GitHub Action. It installs the Count with mise,
keeps his cache between runs, and runs `patchula run`. A repository is updated
when it invites him with a `patchula.pkl` (see [getting
started](/getting-started/)), or when an organization runner supplies its config
with `--config-dir`.

For a handful of repositories this is the simplest setup. For more, or for PR
checkboxes that act straight away, run the [GitHub App server](/guide/server/)
instead.

:::caution[Use a GitHub App token]
Both setups use a GitHub App installation token. Pull requests opened with a
workflow's own `GITHUB_TOKEN` do not trigger other workflows, so your checks
would never run on them and auto-merge would wait forever. The App needs the
permissions listed under [creating the App](/guide/server/#create-the-github-app).
:::

## One repository

The workflow lives in the repository it updates, and the token covers only that
repository.

```yaml
# .github/workflows/patchula.yml
name: Count Patchula

on:
  schedule:
    - cron: "17 3 * * *"
  workflow_dispatch:

permissions: {}

concurrency:
  group: patchula
  cancel-in-progress: false

jobs:
  patchula:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/create-github-app-token@64ab729a6294000f5aa4a4bc0b73d1f32bc45510 # v3.3.0
        id: app
        with:
          client-id: ${{ vars.PATCHULA_CLIENT_ID }}
          private-key: ${{ secrets.PATCHULA_APP_PRIVATE_KEY }}
      - uses: sweepies/count-patchula@v0.5.0
        with:
          token: ${{ steps.app.outputs.token }}
```

## A whole organization

One dedicated repository, such as `my-org/patchula-runner`, runs the Count for
every repository the App is installed on. Install the App on the organization
with access to the repositories you want updated; the runner repository needs
no access of its own. Shared policy can live in the runner repository and be
passed with `--config-dir`.

```yaml
# my-org/patchula-runner/.github/workflows/patchula.yml
name: Count Patchula

on:
  schedule:
    - cron: "17 3 * * *"
  workflow_dispatch:

permissions:
  contents: read

concurrency:
  group: patchula
  cancel-in-progress: false

jobs:
  patchula:
    runs-on: ubuntu-latest
    steps:
      # Only needed for the policy directory below.
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: actions/create-github-app-token@64ab729a6294000f5aa4a4bc0b73d1f32bc45510 # v3.3.0
        id: app
        with:
          client-id: ${{ vars.PATCHULA_CLIENT_ID }}
          private-key: ${{ secrets.PATCHULA_APP_PRIVATE_KEY }}
          # Every repository the App is installed on in this organization.
          owner: ${{ github.repository_owner }}
      - uses: sweepies/count-patchula@v0.5.0
        with:
          token: ${{ steps.app.outputs.token }}
          all-repos: "true"
          # policy/<repo>.pkl or policy/<owner>/<repo>.pkl overrides a
          # repository's own config.
          args: --config-dir policy
```

`--config-dir` is optional. Without it, each repository is configured by its
own `patchula.pkl`, and repositories without one are skipped. With it,
a file in the directory takes the place of that repository's config, so the
runner can opt repositories in without committing to them:

```pkl
// my-org/patchula-runner/policy/my-app.pkl
amends "PatchulaConfig.pkl"

prs { dashboard = true }
```

Policy files amend `PatchulaConfig.pkl` like any config, and the Count supplies it. See
[sharing policy](/guide/configuration/#sharing-policy-across-repositories) to
build these files on one organization module.

A batch across many repositories shares lookups and caches, so one scheduled
run is much cheaper than one per repository.

## Inputs

| Input | Default | |
|---|---|---|
| `token` | | Required. The token the Count reads and writes with |
| `repositories` | this repository | `owner/repo` names, separated by spaces or newlines |
| `all-repos` | `false` | Every repository the token can reach |
| `dry-run` | `false` | Print planned branches and diffs without writing |
| `version` | the action's version | The Count version to install, such as `0.1.1` |
| `args` | | Extra `patchula run` arguments, such as `--config-dir policy` |

The cache, with HTTP validators, write receipts and the lockfile tools, is
saved after every run and restored on the next, so later runs are much faster
than the first.
