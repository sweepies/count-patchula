---
title: "Compatibility with Renovate"
---
The Count covers the managers most repositories on GitHub actually use, and
does them fast. Renovate covers far more ground: more platforms, more managers,
private registries and GitHub's vulnerability alerts. This page says which is which, so
you can tell whether your repositories will notice the switch.

Renovate's lists below come from its public documentation (version 44.174),
never from its source. "Yes" means supported, "partial" says what is missing,
and "no" means not yet. Some of the "no"s are deliberate, and
[departures](/renovate/departures/) explains why.

## Platforms

| Platform | Renovate | Count Patchula |
|---|---|---|
| GitHub and GitHub Enterprise Server | yes | GitHub.com only |
| GitLab, Bitbucket Cloud and Server, Azure DevOps, Gitea, Forgejo | yes | no |
| CodeCommit, Gerrit, SCM-Manager, local | experimental | no |

## Managers

Managers the Count supports, under Renovate's names:

| Renovate | Count Patchula | Notes |
|---|---|---|
| `npm` | `npm` | npm, pnpm, Yarn and Bun lockfiles, catalogs, overrides, `packageManager` |
| `bun` | `bun`, or `npm` | text `bun.lock` only, not `bun.lockb` |
| `nvm`, `nodenv` | `npm` | `.nvmrc`, `.node-version` |
| `github-actions` | `github-actions` | includes runner labels and setup-action version inputs; Forgejo and Gitea workflows too, with no manager of their own |
| `mise`, `asdf` | `mise` | `mise.toml`, `.tool-versions`, `mise.lock` |
| `cargo` | `cargo` | |
| `rust-toolchain` | `cargo` | |
| `gomod` | `go` | indirect requirements too; no `/vN` module path migrations |
| `dockerfile` | `dockerfile` | |
| `docker-compose` | `compose` | |
| `kubernetes` | `kubernetes` | on by default; Renovate needs file patterns |
| `kustomize` | `kustomize` | |
| `flux` | `flux` | |
| `helm-values` | `helm-values` | |
| `helmv3` | `helm` | partial: `Chart.lock` is not regenerated |
| `terraform` | `terraform` | public registry only; no git modules |
| `terraform-version` | `terraform` | |
| `pip_requirements` | `requirements` | |
| `pip-compile` | `requirements` | partial: inputs are read, `pip-compile` is not run |
| `pep621`, `poetry` | `pyproject` | uv and Poetry lockfiles regenerated |
| `pyenv` | `pyproject` | `.python-version` |
| `pre-commit` | `pre-commit` | GitHub, GitLab, Codeberg, Gitea and Forgejo hooks |
| `devcontainer` | `devcontainer` | |
| `woodpecker` | `woodpecker` | anchored images are edited at the anchor |
| `maven` | `maven` | |
| `gradle` | `gradle` | catalogs, scripts, variables, `gradle.properties`; `gradle.lockfile` not regenerated |
| `gradle-wrapper` | `gradle` | the wrapper checksum is refreshed |
| `nuget` | `nuget` | |
| `bundler` | `bundler` | gems without a requirement are left to lockfile maintenance |
| `ruby-version` | `bundler` | |
| `composer` | `composer` | |
| `gitlabci` | `gitlabci` | GitLab CI variables in images are skipped, not aliased |
| `gitlabci-include` | `gitlabci-include` | |
| `regex` | `regex` | Pkl extractors |

Renovate managers with no Count equivalent yet:

`ansible`, `ansible-galaxy`, `ant`, `apm`, `argocd`, `azure-pipelines`, `batect`,
`batect-wrapper`, `bazel`, `bazel-module`, `bazelisk`, `bicep`,
`bitbucket-pipelines`, `bitrise`, `buildkite`, `buildpacks`, `bun-version`,
`cake`, `cdnurl`, `circleci`, `cloudbuild`, `cocoapods`, `conan`, `copier`,
`cpanfile`, `crossplane`, `crow`, `deno`, `deps-edn`, `devbox`, `droneci`,
`fleet`, `fvm`, `git-submodules`, `glasskube`, `gleam`, `haskell-cabal`, `helm-requirements`, `helmfile`, `helmsman`,
`hermit`, `homeassistant-manifest`, `homebrew`, `html`, `jenkins`, `jsonata`,
`jsonnet-bundler`, `kotlin-script`, `leiningen`, `maven-wrapper`, `meteor`,
`mint`, `mix`, `nix`, `ocb`, `osgi`, `pep723`, `pip_setup`, `pipenv`, `pixi`,
`proto`, `pub`, `puppet`, `quadlet`, `runtime-version`, `sbt`, `scalafmt`,
`setup-cfg`, `smithy`, `sveltos`, `swift`, `tekton`, `terragrunt`,
`terragrunt-version`, `tflint-plugin`, `travis`, `typst`, `unity3d`, `vendir`,
`velaci`, `xcodegen`.

`renovate-config` has no equivalent on purpose: the Count's config is Pkl, and
he does not read Renovate presets.

## Datasources

| Renovate | Count Patchula |
|---|---|
| `npm` | `npm` |
| `crate` | `crates` |
| `go` | `go` |
| `pypi` | `pypi` |
| `docker` | `docker`, and `oci` for OCI Helm charts |
| `helm` | `helm` |
| `terraform-provider`, `terraform-module` | `terraform` |
| `github-releases`, `github-tags` | the same, through mise-versions when possible |
| `github-runners` | `github-runners` |
| `gitlab-tags`, `gitea-tags`, `forgejo-tags` | `forge-tags` |
| `maven`, `gradle-version` | `maven`, and the wrapper through `mise` |
| `nuget`, `dotnet-version` | `nuget`, and the SDK through `github-releases` |
| `rubygems`, `ruby-version` | `rubygems`, and Ruby through `mise` |
| `packagist` | `packagist` |
| `node-version`, `golang-version`, `python-version`, `rust-version` | `mise` |

The rest of Renovate's 82 datasources (`git-refs`, `git-tags`, the GitLab and
Gitea release sources, `artifactory`, `conda`, `hex`, `dart`, `pod`, `cdnjs`,
`endoflife-date`, `repology`, `deb`, `rpm`, `apk`, AWS and Azure sources,
`custom` and others) have no equivalent. The Count also has a `mise` source
of his own, listing tools with `mise ls-remote`, so a tool on any backend mise
can list (`conda:`, `gem:`, `go:`, `dotnet:`, `spm:` and others) is looked up
through it. See [mise backends](/renovate/departures/#mise-backends).

## Versioning

| Renovate | Count Patchula |
|---|---|
| `npm`, `cargo`, `semver` | `semver`, with separate npm and Cargo requirement parsers |
| `docker` | `docker` |
| `pep440`, `poetry` | `pep440`, `poetry` |
| `gomod`, `go-mod-directive` | `go` |
| `hashicorp` | `hashicorp` |
| `maven`, `gradle` | `maven` |
| `nuget` | `nuget` |
| `ruby` | `rubygems` |
| `composer` | `composer` |
| `loose` | `loose` |

Renovate's other schemes, including `regex` versioning, `node`, `debian`,
`ubuntu`, `semver-coerced` and `same-major`, have no equivalent.

## Lockfiles

| Lockfile | Renovate | Count Patchula |
|---|---|---|
| `package-lock.json` | npm | aube by default, npm with `npmEngine = "npm"` |
| `pnpm-lock.yaml` | pnpm | aube |
| `yarn.lock` | Yarn | Yarn |
| `bun.lock` | Bun | Bun |
| `bun.lockb` | Bun | no |
| `Cargo.lock`, `go.sum`, `.terraform.lock.hcl` | yes | yes |
| `uv.lock`, `poetry.lock` | yes | yes |
| `packages.lock.json`, `Gemfile.lock`, `composer.lock` | yes | yes |
| `mise.lock` | not listed in its docs | yes |
| `Chart.lock`, `gradle.lockfile` | yes | no |
| `Pipfile.lock`, `pdm.lock` and others | yes | no |

## Features

| Feature | Renovate | Count Patchula |
|---|---|---|
| Config format | JSON or JSON5 with presets | [Pkl](/guide/configuration/), with `amends` and `import` instead of presets |
| Onboarding PR | yes | no; repositories opt in with a config file |
| Config migration PRs | yes | no |
| `packageRules` | yes | `rules`, with typed selectors |
| Grouping | `groupName` | `group` |
| Separate PRs per update type | `separateMajorMinor` and friends | one PR per update line by default |
| `rangeStrategy` | `auto`, `pin`, `bump`, `replace`, `widen`, ... | `versions`: `exact`, `preserve` or `widen` |
| Digest pinning | opt-in | on by default for actions and images |
| `minimumReleaseAge` | off by default | `waitHours`, 24 by default |
| Schedules | natural language and cron | cron `windows` with a timezone |
| PR limits | `prConcurrentLimit`, `prHourlyLimit` | `concurrent`, `perHour`, unlimited by default |
| `rebaseWhen` | several modes | `conflicts` or `behind` |
| Automerge | branch, PR or PR comment, native or not | native auto-merge, or a checked fallback on a later run |
| Dependency dashboard | yes | yes |
| Dashboard approval | yes | create a held-back update from the dashboard |
| Lockfile maintenance | yes | yes |
| Release notes | yes | yes, with mention and backlink suppression |
| Custom regex managers | yes | yes |
| JSONata managers, custom datasources | yes | no |
| Private registries, `hostRules` | yes | no; tool environments are scrubbed |
| `postUpgradeTasks` | yes, admin-allowed | no, on purpose |
| Vulnerability alerts, OSV | yes | OSV.dev advisories mark fixing updates in PRs, labels, the dashboard and the report; see [security advisories](/guide/security/) |
| Merge confidence | Mend beta | no |
| Abandonment detection | yes | no |
| Replacement PRs | yes | no |
| Draft PRs | yes | no |
| Commit signing | configurable author | always API commits signed by GitHub |
| Batch PR by checkbox | no | [yes](/guide/batch/) |
| Machine-readable output | logs | a [versioned JSON report](/reference/report/) |
| Self-hosted server | Renovate CE, the Mend app | [`patchula serve`](/guide/server/) |
| Write updates into a local checkout | no; the local platform is dry-run only | [`patchula apply`](/guide/apply/) and the [terminal UI](/guide/tui/) |

## Performance

On six large public repositories, on the same EC2 instance, the Count was 6.1
times faster than Renovate 44.173.1 cold and 18.5 times faster warm, at a fifth
to a sixteenth of the memory. Coverage, measured as unique dependency names,
matched closely on four of them. The [benchmarks](/benchmarks/) page has the
numbers and the method.
