---
title: "Managers"
---
A manager finds dependencies in one kind of file and knows how to edit them.
Every manager is on by default; `managers` in your config replaces the list,
and `files` replaces one manager's file patterns.

Edits are surgical. Each dependency is extracted with the exact byte range of
its version, and an update rewrites those bytes and nothing else, keeping your
quoting, comments and formatting.

| Manager | Files | Lockfile the Count regenerates |
|---|---|---|
| `npm` | `package.json`, `pnpm-workspace.yaml` and `.yarnrc.yml` catalogs, `.nvmrc`, `.node-version` | `package-lock.json`, `npm-shrinkwrap.json`, `pnpm-lock.yaml`, `yarn.lock`, `bun.lock` |
| `bun` | `package.json` when `npm` is off | `bun.lock` |
| `github-actions` | `.github/workflows/*.yml`, `action.yml` | |
| `mise` | `mise.toml`, `.mise.toml`, `.tool-versions` | `mise.lock` |
| `cargo` | `Cargo.toml`, `rust-toolchain`, `rust-toolchain.toml` | `Cargo.lock` |
| `go` | `go.mod` | `go.sum` |
| `dockerfile` | `Dockerfile`, `Dockerfile.*`, `*.Dockerfile` | |
| `compose` | `compose*.yml`, `docker-compose*.yml` | |
| `kubernetes` | Kubernetes manifests (`*.yaml`) | |
| `kustomize` | `kustomization.yaml` | |
| `flux` | Flux `HelmRepository`, `HelmRelease`, `OCIRepository` | |
| `helm-values` | `values*.yaml` | |
| `helm` | `Chart.yaml` | not yet: `Chart.lock` holds the update |
| `terraform` | `*.tf`, `.terraform-version` | `.terraform.lock.hcl` |
| `requirements` | `requirements*.txt`, `requirements*.in`, `requirements/*.txt` | |
| `pyproject` | `pyproject.toml`, `.python-version` | `uv.lock`, `poetry.lock` |
| `pre-commit` | `.pre-commit-config.yaml` | |
| `devcontainer` | `devcontainer.json`, `.devcontainer.json` | |
| `maven` | `pom.xml` | |
| `gradle` | `*.gradle`, `*.gradle.kts`, `gradle.properties`, `gradle/*.versions.toml`, the wrapper | the wrapper checksum; `gradle.lockfile` holds the update |
| `nuget` | `*.csproj`, `*.fsproj`, `*.vbproj`, `Directory.Packages.props`, `Directory.Build.props`, `global.json`, `.config/dotnet-tools.json` | `packages.lock.json` |
| `bundler` | `Gemfile`, `.ruby-version` | `Gemfile.lock` |
| `composer` | `composer.json` | `composer.lock` |
| `regex` | whatever your [extractors](/guide/configuration/#custom-extractors) select | |

Lockfiles provide the current version where they exist, but their transitive
entries never become dependencies of their own. A file that does not parse at
all, such as an ERB-templated YAML file, is skipped with a warning and the rest
of the repository carries on.

## JavaScript

`npm` covers dependencies, devDependencies, peers, optional dependencies,
aliases (`npm:pkg@^1`), nested overrides, resolutions and `packageManager`.
`workspace:`, `file:`, `link:` and `catalog:` references are local and left
alone. Catalogs are dependencies where they are declared: `catalog` and named
`catalogs` in `pnpm-workspace.yaml`, `.yarnrc.yml` for Yarn 4, and the root
`package.json` for Bun. `.nvmrc` and `.node-version` pin Node; aliases such as
`lts/*` are not versions.

When both `npm` and `bun` are enabled, `npm` owns `package.json` so nothing is
updated twice. Binary `bun.lockb` is not supported; switch to text `bun.lock`.

## GitHub Actions

Workflow steps, reusable workflows, job containers, services and composite
action steps. `run:` text, expressions and local `./` actions are ignored.
Actions and images are pinned to digests by default, keeping the version in a
comment. Setup actions' version inputs (`node-version`, `go-version`,
`bun-version`, golangci-lint and goreleaser versions) are tools in their own
right. Hosted runner labels in `runs-on` upgrade within their OS and
architecture family. Flow-style YAML and YAML aliases at a version location are
not editable yet and report an error.

## mise

Strings, arrays, version tables and `.tool-versions`. Short names resolve
through mise's published registry to their real source (aqua, GitHub, npm,
cargo, PyPI and the core tools), and unknown aliases produce a lookup warning.
Plugins are never run. An existing `mise.lock` is regenerated with the update.

## Cargo, Go and Terraform

Cargo reads workspace, development, build and target tables and renamed
crates, and resolves from the nearest `Cargo.lock`. A member's
`workspace = true` points at the root declaration, which is where the edit
goes. `rust-toolchain(.toml)` pins Rust.

Go reads `require` blocks and single directives, plus the `go` directive (kind
`go-directive`, the minimum language version) and `toolchain`. Indirect
requirements are maintained too.

Terraform reads providers, registry modules and the provider lock with a real
HCL parser; `.terraform-version` pins Terraform itself.

## Containers and Kubernetes

Dockerfiles support global `ARG` substitution, continuations, `--platform` and
stage aliases; `scratch` and references to earlier stages are skipped. Compose
reads each service's `image`. Kubernetes reads workload images, Kustomize its
`images` and `helmCharts` transforms, Flux its repositories and releases, and
Helm values files their `repository`/`tag`/`digest` maps. ConfigMaps, Secrets
and annotations are not dependencies. Templated images need concrete values;
Helm templates are not rendered. Image updates keep the tag's variant, so
`3.12-slim` never becomes `3.13-alpine`.

## Python

`requirements` reads pip requirement files, keeping extras, markers and hashes
outside the version. `pyproject` reads PEP 621 dependencies and optional
dependencies, PEP 735 dependency groups, uv's dev, constraint and override
dependencies, and Poetry's dependencies and groups. Edits touch only the
version specifier. URL requirements, unversioned names and Poetry `git`/`path`
sources are skipped. Names are PEP 503 normalised and resolved from `uv.lock` or
`poetry.lock`; `.python-version` pins CPython.

## Helm, pre-commit and devcontainers

`helm` reads `Chart.yaml` dependencies from HTTPS and `oci://` repositories.
`pre-commit` updates each hook repository's `rev` on GitHub, GitLab, Codeberg
and Gitea/Forgejo hosts, skipping `local` and `meta` hooks and bare commit pins.
`devcontainer` reads the `image` and OCI `features`, whose version lives in the
feature key (`ghcr.io/devcontainers/features/node:1`).

## Maven and Gradle

Maven reads `parent`, `dependencies`, `dependencyManagement`, build `plugins`,
`pluginManagement` and `extensions`. A `${property}` version is edited where the
property is defined. Gradle reads version catalogs, literal coordinates,
`group:/name:/version:` notation and `id "…" version "…"` plugins in every
Gradle script, and resolves versions kept in variables: `ext` blocks,
`def`/`val` locals, `extra[...]`, map literals such as `versions += [jackson:
"2.15.0"]`, and `gradle.properties`. Under a `src/` directory only `src/main/` is
read, since test resources hold fixtures. A name defined twice, computed values
and the project's own `version` are skipped, as are packages reached through
several variables (deliberate parallel versions, such as Kafka's compatibility
pins). The wrapper's `distributionUrl` upgrades, and a recorded
`distributionSha256Sum` is replaced with Gradle's published checksum.

Versions keep their release variant: Guava's `33.4.8-android` never becomes
`33.7.2-jre`.

## .NET, Ruby and PHP

`nuget` reads `PackageReference`, `PackageVersion` and `GlobalPackageReference`
items, local tools and the SDK in `global.json` (feature-band releases only).
`$(Property)` and floating `*` versions are skipped. Lockfiles are restored
with `dotnet restore --force-evaluate`.

`bundler` reads `gem "name", "requirement"` lines and the `ruby` directive,
resolving from `Gemfile.lock`, plus `.ruby-version`. Gems from `git:`, `path:`
or `github:` sources and gems with two requirement strings are skipped, and so
are gems with no requirement at all: any version satisfies them, so only
[lockfile maintenance](/guide/lockfiles/#lockfile-maintenance) moves them.
Evaluating a Gemfile runs Ruby, so `bundle lock` runs in a scrubbed
environment that ignores any repository `.bundle/config`.

`composer` reads `require` and `require-dev`, skipping platform packages
(`php`, `ext-*`, `lib-*`), `dev-` branches and `*`. `composer update` runs with
`--no-install --no-scripts --no-plugins`, so no repository code runs. The PHP
version constrains resolution; extension requirements do not, since they
describe where the app is deployed, not the runner.

## Kinds

Every dependency has a kind, which rules can select: `production`,
`development`, `peer`, `optional` and `build` for packages; `action`,
`reusable-workflow` and `runner` for workflows; `container-image`; `tool` and
`toolchain` for mise tools and pinned toolchains; `go-directive`; `provider`
and `module` for Terraform; and `chart` for Helm.
