---
title: "Compatibility with Renovate"
---
This page maps a Renovate setup onto the Count: which managers, datasources,
versioning schemes, lockfiles and options have an equivalent, under which name,
and which have none. [Migrating a config](/renovate/migrating/) translates the
options one by one, and [departures](/renovate/departures/) lists the defaults
that differ.

Renovate's names come from its public documentation (version 44.174). "Partial"
says what is missing.

## Platforms

The Count runs on GitHub.com only. GitHub Enterprise Server, GitLab, Bitbucket,
Azure DevOps, Gitea, Forgejo and Renovate's experimental platforms have no
equivalent.

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
| `copier` | `copier` | partial: newer tags are reported, never applied by a run ([why](/renovate/departures/#copier-templates)). GitHub, GitLab, Gitea and Forgejo templates only |
| `maven` | `maven` | |
| `gradle` | `gradle` | catalogs, scripts, variables, `gradle.properties`; `gradle.lockfile` not regenerated |
| `gradle-wrapper` | `gradle` | the wrapper checksum is refreshed |
| `nuget` | `nuget` | |
| `bundler` | `bundler` | gems without a requirement are left to lockfile maintenance |
| `ruby-version` | `bundler` | |
| `composer` | `composer` | |
| `gitlabci` | `gitlabci` | GitLab CI variables in images are skipped, not aliased |
| `gitlabci-include` | `gitlabci-include` | |
| `nix` | `nix` | on by default; inputs resolved by nix itself, not `git-refs` |
| `apm` | `apm` | `apm.lock.yaml` refreshed with `apm lock`; commit pins by default |
| `regex` | `regex` | Pkl extractors |

Renovate managers with no Count equivalent yet:

`ansible`, `ansible-galaxy`, `ant`, `argocd`, `azure-pipelines`, `batect`,
`batect-wrapper`, `bazel`, `bazel-module`, `bazelisk`, `bicep`,
`bitbucket-pipelines`, `bitrise`, `buildkite`, `buildpacks`, `bun-version`,
`cake`, `cdnurl`, `circleci`, `cloudbuild`, `cocoapods`, `conan`, `cpanfile`,
`crossplane`, `crow`, `deno`, `deps-edn`, `devbox`, `droneci`,
`fleet`, `fvm`, `git-submodules`, `glasskube`, `gleam`, `haskell-cabal`, `helm-requirements`, `helmfile`, `helmsman`,
`hermit`, `homeassistant-manifest`, `homebrew`, `html`, `jenkins`, `jsonata`,
`jsonnet-bundler`, `kotlin-script`, `leiningen`, `maven-wrapper`, `meteor`,
`mint`, `mix`, `ocb`, `osgi`, `pep723`, `pip_setup`, `pipenv`, `pixi`,
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
| `git-refs`, for Nix flake inputs | `nix`, through `nix flake prefetch` |

The rest of Renovate's 82 datasources (`git-refs` other than for Nix, `git-tags`, the GitLab and
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

| Lockfile | Count Patchula |
|---|---|
| `package-lock.json` | aube by default, npm with `npmEngine = "npm"` |
| `pnpm-lock.yaml` | aube |
| `yarn.lock`, `bun.lock` | Yarn, Bun |
| `Cargo.lock`, `go.sum`, `.terraform.lock.hcl` | yes |
| `uv.lock`, `poetry.lock` | yes |
| `packages.lock.json`, `Gemfile.lock`, `composer.lock` | yes |
| `mise.lock`, `flake.lock`, `apm.lock.yaml` | yes |
| `bun.lockb`, `Chart.lock`, `gradle.lockfile`, `Pipfile.lock`, `pdm.lock` and others | no |

## Options and features

| Renovate | Count Patchula |
|---|---|
| JSON or JSON5 config, `extends` and presets | [Pkl](/guide/configuration/), with `amends` and `import`; no presets |
| Onboarding PR | none; repositories opt in with a config file |
| Config migration PRs | none |
| `packageRules` | `rules`, with typed selectors |
| `groupName` | `group` |
| `separateMajorMinor` and friends | one PR per update line by default |
| `rangeStrategy` | `versions`: `exact`, `preserve` or `widen` |
| `pinDigests` | `digest`, on by default for actions and images |
| `minimumReleaseAge` | `waitHours`, 24 by default |
| `schedule`, `timezone` | cron `windows` with a zone; no natural-language schedules |
| `prConcurrentLimit`, `prHourlyLimit` | `concurrent`, `perHour`, unlimited by default |
| `rebaseWhen` | `rebase`: `conflicts` or `behind` |
| `automerge` | `merge`: native auto-merge, or a checked fallback on a later run, with [tooling warnings](/guide/auto-merge/#tooling-warnings) when CI, required checks or delivery are missing |
| `dependencyDashboard`, dashboard approval | `dashboard`; a held-back update can be created from it |
| `lockFileMaintenance` | `lockfiles { refresh }` |
| `fetchChangeLogs` | `releaseNotes` |
| `customManagers` (regex) | `extractors` |
| `vulnerabilityAlerts`, `osvVulnerabilityAlerts` | OSV.dev [advisories](/guide/security/), on by default |
| none, for OS packages in images | [image rebuild advisories](/guide/security/#container-image-rebuilds): a new digest is compared with the old through their SBOMs and OSV.dev |
| JSONata managers, custom datasources | none |
| `hostRules`, private registries | none; tool environments are scrubbed |
| `postUpgradeTasks` | none, on purpose |
| `gitAuthor` | none; commits are made through the API and signed by GitHub |
| Merge confidence, abandonment detection, replacement PRs, draft PRs | none |
