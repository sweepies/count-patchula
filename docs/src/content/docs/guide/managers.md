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
| `github-actions` | `.github/workflows/*.yml`, `.forgejo/workflows/*.yml`, `.gitea/workflows/*.yml`, `action.yml` | |
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
| `woodpecker` | `.woodpecker.yml`, `.woodpecker.yaml`, `.woodpecker/*.yml` and `*.yaml` | |
| `copier` | `.copier-answers.yml`, `.copier-answers.yaml`, `.copier-answers.*.yml` and `*.yaml` | none: newer template versions are [reported, not applied](#copier) |
| `maven` | `pom.xml` | |
| `gradle` | `*.gradle`, `*.gradle.kts`, `gradle.properties`, `gradle/*.versions.toml`, the wrapper | the wrapper checksum; `gradle.lockfile` holds the update |
| `nuget` | `*.csproj`, `*.fsproj`, `*.vbproj`, `Directory.Packages.props`, `Directory.Build.props`, `global.json`, `.config/dotnet-tools.json` | `packages.lock.json` |
| `bundler` | `Gemfile`, `.ruby-version` | `Gemfile.lock` |
| `composer` | `composer.json` | `composer.lock` |
| `gitlabci` | `.gitlab-ci.yml`, `*.gitlab-ci.yml` (and `.yaml`) | |
| `gitlabci-include` | `.gitlab-ci.yml`, `*.gitlab-ci.yml` (and `.yaml`) | |
| `nix` | `flake.lock` beside a `flake.nix` | `flake.lock` |
| `regex` | whatever your [extractors](/guide/configuration/#custom-extractors) select | |

Lockfiles provide the current version where they exist. Their transitive
entries only become dependencies of their own, of kind `transitive`, when they
have a [known vulnerability](/guide/security/#lockfile-only-packages). A file
that does not parse at all, such as an ERB-templated YAML file, is skipped with
a warning and the rest of the repository carries on.

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

### Forgejo and Gitea Actions

Workflows in `.forgejo/workflows/` and `.gitea/workflows/` at the repository
root are read the same way, by the same `github-actions` manager, and pinned
and updated the same way. An action can be named by URL there, such as
`https://code.forgejo.org/actions/setup-forgejo@v3`. A URL on `github.com` is
looked up as GitHub tags; a URL on a Gitea or Forgejo host is looked up as
`forge-tags` on that host, and the Count keeps the URL when he rewrites the
ref. Hosts are recognised by name: `codeberg.org`, `gitea.com`, and hosts
with a `gitea` or `forgejo` label such as `code.forgejo.org` or
`git.forgejo.example.com`. Actions on other hosts are skipped.

A bare `owner/repo` ref is resolved by the runner against the instance's
default action host, which only the instance's `app.ini` knows. The Count
uses each project's documented default:

| File | Bare `owner/repo` looked up on |
|---|---|
| `.github/workflows/`, `.gitea/workflows/`, `action.yml` | GitHub, Gitea's default (`DEFAULT_ACTIONS_URL = github`) |
| `.forgejo/workflows/`, `.forgejo/**/action.yml` | `data.forgejo.org`, Forgejo's default |

If your instance changes `DEFAULT_ACTIONS_URL`, write the actions it should
not resolve this way as URLs. That is what Forgejo recommends anyway. Forge
digests come from the host's tag list, so only tag refs are pinned there:
a branch ref such as `@main` on a forge is left alone. Runner labels in
`.forgejo/` and `.gitea/` workflows name the instance's own runners, so they
are not updated.

## mise

Strings, arrays, version tables and `.tool-versions`. The Count asks mise
itself for each tool's versions (`mise ls-remote --json`), so a tool is found
wherever mise finds it: a short registry name such as `jq`, or an identifier for
any backend that reads release metadata (`aqua`, `github`, `gitlab`,
`forgejo`, `packslip`, `npm`, `pypi`, `cargo`, `go`, `gem`, `dotnet`,
`conda`, `spm`, `http`, `s3` and `core`). That includes every form of
packslip source: `packslip:owner/repo`, `packslip:github.com/owner/repo`, a
tool inside a monorepo (`packslip:github.com/owner/repo/tools/name`) and a
tool on a publisher's domain. An existing `mise.lock` is regenerated with the
update. mise runs in a [sandbox](/guide/sources/#mise-lookups), with the
Count's own pinned mise and none of your configuration.

Some tools are not looked up, each with a lookup warning:

- `asdf:` and `vfox:` tools, and registry names whose only backend is an asdf
  or vfox plugin, because those backends run plugin code. A short name that
  lists a plugin first and another backend after it uses the other backend.
- Names with tool options in brackets, such as `pypi:black[extras=jupyter]`.
  Options can point a backend at another host, so the Count passes none, and
  mise's options in a table (`{ version = "1", asset_pattern = "..." }`) are
  not read either.
- Backends that install from a Git ref rather than from releases, such as
  `cargo:https://github.com/owner/repo`: mise lists only `HEAD` for them, so
  there is no version to update to, including with a `tag:` pin.

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

## Woodpecker

`woodpecker` reads the images of steps, services and the `clone` step, under
`steps:` or the older `pipeline:` key, written as a map or a list. Only the
root `.woodpecker.yml` or `.woodpecker.yaml` and files directly inside
`.woodpecker/` are workflows. Images, plugins included, are pinned to digests
by default like any other container image. An image defined once under a YAML
anchor and aliased by several steps is one dependency, edited at the anchor;
one merged into a step with `<<: *anchor` is read too.

An image built from a variable, such as `golang:${GO_VERSION}` from the
workflow's `matrix` or `${REGISTRY}/tool:1` from the environment, is only
known when the workflow runs, so the Count skips it. Each skipped image is
listed with its line and the reason, in the run summary and under `skipped` in
the [JSON report](/reference/report/). Unlike a warning, a skipped image
doesn't stop the Count from closing obsolete branches.

## Copier

`copier` reads the template a project was generated from: `_src_path` and the
tag in `_commit` of each answers file. Templates on GitHub (`gh:` included)
are looked up as `github-tags`, and templates on GitLab (`gl:` included),
Gitea and Forgejo as `forge-tags`, compared as `pep440` versions. SSH URLs
are looked up over the same host's API. A template on any other host, a local
template, or a `_commit` that is not a release tag (such as
`v1.4.0-5-g1a2b3c4`) is skipped, and listed with the reason in the run summary
and the [JSON report](/reference/report/).

A newer tag is reported, never proposed: the run summary, the JSON report
under `notices`, the [dashboard](/guide/dashboard/) and the
[TUI](/guide/tui/#templates) show "template gh:acme/template: v1.4.0 → v1.6.0
available", with a compare link where the host has one. The release-age wait
and rules apply as usual, and the kind is `template`. The Count never opens a
pull request for it and never edits `_commit`: only `copier update` applies a
template's changes, and a bare `_commit` bump would make the next update start
from the wrong version and drop them. He runs copier only where you ask him
to, on your own machine: from the [TUI](/guide/tui/#templates) or with
[`patchula apply --copier`](/guide/apply/#copier-templates). See the
[departure](/renovate/departures/#copier-templates).

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

## GitLab CI

`gitlabci` reads the `image` and `services` of every job, `default` and hidden
`.template` jobs, as a string or as a `name:` mapping. Images are pinned to
digests by default, like images everywhere else, and GitLab's own registry,
`registry.gitlab.com`, is read with the anonymous tokens gitlab.com issues. It
also updates CI/CD component references
(`include: - component: gitlab.com/components/sast/sast@3.4.0`) from the
component project's tags. `~latest` and commit SHAs are left alone.

`gitlabci-include` updates the `ref` of `include: project:` entries from that
project's tags. Refs and component versions are written as the tag is named,
since GitLab loads them by exact name. Entries without a `ref`, branch names and commit SHAs are left
alone, and so are `local`, `remote` and `template` includes. A ref or
component reference shared through a YAML anchor is skipped, since one tag may
not exist in every project that uses it; an anchored image is edited once, at
the anchor.

Project includes resolve on `gitlab.host`, gitlab.com by default, and so does
`$CI_SERVER_FQDN` (or `$CI_SERVER_HOST`) at the start of a component reference.
A self-hosted instance works when it has a `gitlab` label in its host name,
such as `gitlab.example.com`, whether it is named in the component reference or
in config:

```pkl
gitlab { host = "gitlab.example.com" }
```

Other GitLab CI variables are not expanded. An image such as
`$CI_REGISTRY_IMAGE:latest` or `${CI_DEPENDENCY_PROXY_GROUP_IMAGE_PREFIX}/node:22`
is skipped, and listed with its line and the reason in the run summary and
under `skipped` in the [JSON report](/reference/report/). Unlike a warning, a
skipped reference leaves branch cleanup and auto-merge on.

## Helm, pre-commit and devcontainers

`helm` reads `Chart.yaml` dependencies from HTTPS and `oci://` repositories.
`pre-commit` updates each hook repository's `rev` on GitHub, GitLab, Codeberg
and Gitea/Forgejo hosts, skipping `local` and `meta` hooks and bare commit pins.
A `rev` is written as the tag is named (`v1.0` becomes `v1.1`, not `v1.1.0`),
since pre-commit checks it out by that name.
`devcontainer` reads the `image` and OCI `features`, whose version lives in the
feature key (`ghcr.io/devcontainers/features/node:1`).

## Maven and Gradle

Maven reads `parent`, `dependencies`, `dependencyManagement`, build `plugins`,
`pluginManagement` and `extensions`. A `${property}` version is edited where the
property is defined.

Gradle reads version catalogs, literal coordinates, `group:/name:/version:`
notation and `id "…" version "…"` plugins in every Gradle script. It resolves
versions kept in `ext` blocks, `def`/`val` locals, `extra[...]`, map literals
such as `versions += [jackson: "2.15.0"]`, and `gradle.properties`.

Gradle skips anything under `src/` except `src/main/`, since test resources
hold fixtures; a name defined twice; computed values; the project's own
`version`; and packages reached through several variables, which are
deliberate parallel versions such as Kafka's compatibility pins. The wrapper's
`distributionUrl` upgrades, and a recorded `distributionSha256Sum` is replaced
with Gradle's published checksum.

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

## Nix

`nix` reads the `flake.lock` next to each `flake.nix`. Every direct input of
the root flake is a dependency named after the input, such as `nixpkgs`, with
its fully qualified root URL as the package (`https://github.com/NixOS/nixpkgs`)
and the reference it was written with as the current value
(`github:NixOS/nixpkgs/nixos-unstable`). Inputs that only `follows` another one
have no lock of their own and are not dependencies. Transitive inputs move with
the input that brings them in.

The Count asks nix what each reference points at now, with
`nix flake prefetch`, and proposes the new revision as a `digest` update dated
by the commit's `lastModified`, so the release-age wait applies. The branch,
`patchula/nix-nixpkgs`, runs `nix flake update nixpkgs`, and the lock nix
writes is the whole change; `flake.nix` is never edited. An input pinned to a
tag or commit in its URL, such as `github:numtide/flake-utils/v1.0.0`, does not
move, the same as with nix itself.

Inputs nix would fetch from outside the repository are skipped with a reason:
`path:` and `git+file:` inputs that leave the repository, ssh URLs, plain
`http`, and Mercurial. A `path:` input inside the repository changes with the
repository, so it is skipped too. After nix runs, every node it added or changed
in the lock is checked the same way, and the branch fails rather than publish
a lock that reaches outside. How nix runs is under
[Lockfiles](/guide/lockfiles/#nix).

## Kinds

Every dependency has a kind, which rules can select: `production`,
`development`, `peer`, `optional` and `build` for packages; `action`,
`reusable-workflow` and `runner` for workflows, with GitLab CI includes and
components as `reusable-workflow`; `container-image`; `tool` and
`toolchain` for mise tools and pinned toolchains; `go-directive`; `provider`
and `module` for Terraform; `chart` for Helm; `template` for Copier;
`flake-input` for Nix; and
`transitive` for a
[vulnerable lockfile-only package](/guide/security/#lockfile-only-packages).
[Reference data](/guide/configuration/#reference-data) also has the kind
`reference-data`, on top of its own.
