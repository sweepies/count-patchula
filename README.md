<div align="center">
  <img src="docs/public/logo.png" width="180" alt="Count Patchula, a bat holding a wrench">
  <h1>Count Patchula</h1>
  <p><strong>Count Patchula vants to update your deps. He never sleeps. Neither do your CVEs.</strong></p>
  <p>A dependency update bot for GitHub that does a whole batch of repositories in one fast, cached run.</p>
  <p>
    <a href="docs/getting-started.md">Get started</a> ·
    <a href="docs/">Documentation</a> ·
    <a href="docs/renovate/compatibility.md">Coming from Renovate</a> ·
    <a href="docs/benchmarks.md">Benchmarks</a>
  </p>
</div>

The Count reads your repositories' manifests through the GitHub API, looks up
newer versions, and opens one pull request per update, with a regenerated
lockfile, release notes and a commit signed by GitHub. One process handles the
whole batch: repositories run concurrently, every package is looked up once
however many repositories use it, and a repository that has not changed costs
one GraphQL query on the write side.

## Get started

```sh
cargo install --git https://github.com/sweepies/count-patchula --locked patchula-cli
```

Invite the Count into a repository with `.github/patchula.pkl`:

```pkl
amends "../Patchula.pkl"

prs { dashboard = true }
```

Then, with a GitHub App installation token in `PATCHULA_TOKEN`:

```sh
patchula run --dry-run sweepies/my-app   # look first
patchula run sweepies/my-app             # then let him bite
```

Lockfile tools come from [mise](https://mise.jdx.dev), so it needs to be on the
`PATH`. See [getting started](docs/getting-started.md).

## What you get

- Real lockfiles for npm, pnpm, Yarn, Bun, Cargo, Go, Terraform, uv, Poetry,
  NuGet, Bundler, Composer and mise, regenerated with the real tools. npm and
  pnpm locks go through [aube](https://aube.sh), which is ten to twenty times
  faster than npm and pnpm at it.
- Managers for JavaScript, GitHub Actions, mise, Rust, Go, Docker, Compose,
  Kubernetes, Kustomize, Flux, Helm, Terraform, Python, Maven, Gradle, .NET,
  Ruby, PHP, pre-commit, devcontainers, and your own regexes.
- Typed configuration in [Pkl](https://pkl-lang.org). A typo is an error with a
  line number, not a silently ignored key.
- Careful defaults: exact pins, digest-pinned actions and images, a 24-hour
  release-age wait, and nothing auto-merges until you say so.
- A batch PR you fill by ticking a box on any update PR, keeping the version
  you reviewed.
- A dependency dashboard, schedule windows, PR limits, grouping, auto-merge
  behind check gates, and a GitHub App server mode driven by webhooks.
- A terminal UI (`patchula tui`) to browse updates, and with `--local`, pick
  some and write them into your files.

## How fast

Against Renovate 44.173.1 on six large public repositories (mastodon, sentry,
grafana, monica, jellyfin, kafka), each tool alone on the same c7i.xlarge:

| | Count Patchula | Renovate |
|---|---|---|
| All six, cold | 150.5 s | 920.6 s |
| All six, warm | 14.8 s | 274.3 s |
| Peak memory, grafana cold | 494 MB | 3.6 GB |

The [benchmarks page](docs/benchmarks.md) has
the method and every run.

## Coming from Renovate

The Count is not a Renovate clone, and he only works on GitHub. The
[compatibility matrix](docs/renovate/compatibility.md)
lists every manager, datasource and feature, and [migrating a
config](docs/renovate/migrating.md) maps
`renovate.json` options onto Pkl.

## Development

Install [mise](https://mise.jdx.dev), then `mise trust` and `mise install`.

```sh
mise run build      # build the workspace
mise run test       # offline tests
mise run lint       # rustfmt and Clippy, warnings denied
mise run ci         # fixtures check, lint, test and build
mise run docs:dev   # the documentation site
```

The site is deployed to Cloudflare Workers by Workers Builds, Cloudflare's
GitHub integration. It installs mise and runs `mise run docs:deploy` on `main`
and `mise run docs:upload` on other branches, so each pull request that touches
`docs/` or `mise.toml` gets a preview.

Builds go through [mr boxington](https://mr-boxington.jdx.dev) for a shared
Cargo cache. Tests never touch the network: they replay HTTP fixtures the Count
recorded himself. Tools only the tests need, like the lockfile engines, are
task-scoped, so a plain `mise install` does not fetch them.

## Clean-room rule

Count Patchula is an independent implementation under the MIT licence. Renovate
(AGPL-3.0) may be run as a black box to compare results, but its source code,
preset data and test fixtures are never read, copied or adapted into this
repository.

## License

MIT
