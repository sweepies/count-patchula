<div align="center">
  <img src="docs/public/logo.png" width="180" alt="Count Patchula, a bat holding a wrench">
  <h1>Count Patchula</h1>
  <p><strong>Count Patchula vants to update your deps. He never sleeps. Neither do your CVEs.</strong></p>
  <p>A modern dependency updater with strong defaults.</p>
  <p>
    <a href="https://count-patchula.sweepy.dev/getting-started/">Get started</a> ·
    <a href="https://count-patchula.sweepy.dev">Documentation</a> ·
    <a href="https://count-patchula.sweepy.dev/renovate/compatibility/">Coming from Renovate</a> ·
    <a href="https://count-patchula.sweepy.dev/benchmarks/">Benchmarks</a>
  </p>
</div>

The Count reads your repositories' manifests, looks up newer versions, and
opens one pull request per update, with release notes. One process handles the
whole batch: repositories run concurrently, and every package is looked up once
however many repositories use it.

## Get started

```sh
mise use -g packslip:sweepies/count-patchula
```

Other ways to install him are on the [installation page](https://count-patchula.sweepy.dev/installation/).

Invite the Count into a repository with `patchula.pkl`:

```pkl
amends "PatchulaConfig.pkl"

prs { dashboard = true }
```

Then, with a GitHub App installation token in `PATCHULA_TOKEN`:

```sh
patchula run --dry-run acme/my-app   # look first
patchula run acme/my-app             # then let him bite
```

Lockfile tools, and the versions of the tools in `mise.toml`, come from
[mise](https://mise.jdx.dev), so it needs to be on the `PATH`. See [getting started](https://count-patchula.sweepy.dev/getting-started/).

The CLI is the quickest way to try him, and the [GitHub
Action](https://count-patchula.sweepy.dev/guide/github-actions/) runs him on a
schedule. The recommended way to run him is the [GitHub App
server](https://count-patchula.sweepy.dev/guide/server/): one long-running
process keeps every cache warm, batches work across repositories, and acts on
PR checkboxes the moment you tick them. It idles cheaply between batches, so
usage-billed hosts such as Railway suit it well. Each release publishes it as
a [container image](https://count-patchula.sweepy.dev/guide/server/#container-image),
`ghcr.io/sweepies/count-patchula`, for linux/amd64 and linux/arm64. Once it is up, `patchula run
--remote` runs on it from your machine, signed in with GitHub, with no
token or toolchain of your own.

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
  release-age wait, and nothing auto-merges until you say so, except CA root
  bundles and other reference data, which are only useful while current.
- A batch PR you fill by ticking a box on any update PR, keeping the version
  you reviewed.
- A dependency dashboard, schedule windows, PR limits, grouping, auto-merge
  behind check gates, and a GitHub App server mode driven by webhooks.
- A terminal UI (`patchula tui`) to browse updates, and in a checkout, pick
  some and write them into your files.

## How fast

Against Renovate 44.173.1 on six large public repositories (mastodon, sentry,
grafana, monica, jellyfin, kafka), each tool alone on the same c7i.xlarge:

| | Count Patchula | Renovate | |
|---|---|---|---|
| All six, cold | **150.5 s** | 920.6 s | 6.1× faster |
| All six, warm | **14.8 s** | 274.3 s | 18.5× faster |
| Peak memory, grafana cold | **494 MB** | 3.6 GB | 7.3× less |

The [benchmarks page](https://count-patchula.sweepy.dev/benchmarks/) has
the method and every run.

## Coming from Renovate

The [compatibility matrix](https://count-patchula.sweepy.dev/renovate/compatibility/)
lists every manager, datasource and feature, and [migrating a
config](https://count-patchula.sweepy.dev/renovate/migrating/) maps
`renovate.json` options onto Pkl.

## Development

Install [mise](https://mise.jdx.dev), then `mise trust` and `mise install`.

```sh
mise run build      # build the workspace
mise run test       # offline tests
mise run lint       # rustfmt, Clippy (warnings denied) and jactionlint
mise run ci         # fixtures check, lint, test and build
mise run docs:dev   # the documentation site for this checkout
```

Builds go through [mr boxington](https://mr-boxington.jdx.dev) for a shared
Cargo cache. Tests never touch the network: they replay HTTP fixtures the Count
recorded himself. Tools only the tests need, like the lockfile engines, are
task-scoped, so a plain `mise install` does not fetch them.

Count Patchula is an independent implementation under the MIT licence. Renovate
(AGPL-3.0) may be run as a black box to compare results, but its source code,
preset data and test fixtures are never read, copied or adapted into this
repository.

## Thanks

Count Patchula would not exist without [@jdx](https://github.com/jdx). His
work inspired it, and it is built on his tools at every step:
[mise](https://mise.jdx.dev) installs every toolchain and lockfile engine,
[aube](https://aube.sh) writes the npm and pnpm locks,
[mr boxington](https://mr-boxington.jdx.dev) caches the builds,
[usage](https://usage.jdx.dev) generates the CLI reference and completions,
[tak](https://github.com/jdx/tak) runs the benchmarks,
[communique](https://communique.jdx.dev) writes the release notes, and
[packslip](https://packslip.dev) signs the releases. Thank you, Jeff.

## License

MIT
