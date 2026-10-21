# GitHub Actions

The repository is also a GitHub Action. It installs the Count with mise,
keeps his cache between runs, and runs `patchula run`.

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
      - uses: sweepies/count-patchula@v0.1.1
        with:
          token: ${{ steps.app.outputs.token }}
```

The repository still needs a `.github/patchula.pkl` to invite him; see
[getting started](/getting-started).

## The token

Use a GitHub App installation token. Pull requests opened with the workflow's
own `GITHUB_TOKEN` do not trigger other workflows, so your checks would never
run on them, and auto-merge would wait forever. The App needs the permissions
listed under [creating the App](/guide/server#create-the-github-app).

`create-github-app-token` scopes the token to the current repository. Give it an
`owner` to cover every repository the App is installed on, and pass
`all-repos: true`.

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
than the first. For a large batch or many runs a day, the [GitHub App
server](/guide/server) keeps everything warm in one process instead.
