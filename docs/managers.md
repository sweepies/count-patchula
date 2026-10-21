# Extraction (SWE-99)

Managers operate on supplied UTF-8 files and return typed dependencies with byte
ranges, line/column, semantic paths, source and optional resolved version/digest.
Repository selection scans package.json files throughout workspaces, including
nested manifests; lock resolution walks from the workspace to its ancestors.
Lockfiles are parsed once per extraction. Transitive lockfile entries are not
emitted as manifest dependencies. npm takes precedence when both npm and bun
are enabled, avoiding duplicate manifest updates.

npm supports production/dev/peer/optional dependencies, aliases, nested overrides,
resolutions and packageManager. Workspace/file/link/catalog refs are local and
not registry dependencies. Catalog entries are dependencies where they are
declared: `catalog` and named `catalogs` in `pnpm-workspace.yaml` (pnpm),
`.yarnrc.yml` (Yarn 4) and the root `package.json`, top level or under
`workspaces` (Bun). YAML catalogs keep their quoting and comments. `.nvmrc` and
`.node-version` pin Node with a numeric version, optionally `v`-prefixed;
aliases such as `lts/*` are not versions. These files have no lockfile job. bun.lock JSONC and npm lock versions 1–3 provide
resolved versions. Binary bun.lockb remains unsupported, and pnpm and Yarn
locks are not parsed for resolved versions; the lockfile runner regenerates them
with aube and Yarn instead.

Workflow extraction follows jobs, steps, reusable workflows, services and
composite action steps, ignoring run text, expressions and local refs. SHA pins
expose an optional version comment as their current value and the SHA as their
digest. Images retain tags/digests and use registry v2 lookup.
Workflow flow-style and YAML alias scalar locations currently fail
explicitly; normal block YAML is supported. Regex extractors use the Pkl model.
Literal hosted runner labels in runs-on scalars, arrays and labels mappings have
kind `runner`, source `github-runners`, and an OS/architecture family identity.
Self-hosted jobs, grouped organizational runners and expressions are skipped.

mise supports strings, arrays, version tables and .tool-versions. Published
registry shorthands and explicit backend identifiers resolve to real sources;
numeric extracted values fill current.version without a lookup. Floating
selectors retain no fabricated resolved version. Unknown aliases retain the mise
source and cause a lookup warning. Runtime options in tables are not dependencies.
When a catalog is available, current.version follows the backend's release
identity: aqua strips an optional v, Bun strips bun-v, Go strips go, and ordinary
GitHub backends retain their tag. This resolution ignores age/yanked filters.

References used for independent tests: [package.json](https://docs.npmjs.com/cli/v11/configuring-npm/package-json),
[Bun lockfile](https://bun.sh/docs/pm/lockfile),
[workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax),
[mise configuration](https://mise.jdx.dev/configuration.html).

Wave 2 adds Cargo workspace/development/build/target tables, renamed crates, and
nearest-lock resolution. Inherited workspace declarations retain their member
reference location; the editable version lives at the root declaration. Cargo
and HCL documents/locks are parsed once per repository extraction. Lockfiles
enrich direct declarations without emitting transitive entries.

Dockerfile supports global ARG substitution, continuations, --platform and stage
aliases; scratch/stage references are skipped. Compose reads services' images.
Go extracts require blocks/single directives, with separate `go-directive` and
`toolchain` kinds resolved through Go's mise backend. Terraform reads provider
maps, registry modules and provider lock resolutions using a real HCL parser.
Requirements retain extras/markers/hashes outside their constraint location.

`pyproject` reads PEP 621 `[project]` dependencies and optional dependencies,
PEP 735 `[dependency-groups]`, uv's `dev-dependencies`,
`constraint-dependencies` and `override-dependencies`, and Poetry's
`dependencies`, `dev-dependencies` and `group.*.dependencies`. PEP 508 strings
are edited at their version specifier only, leaving names, extras and markers;
strings with escapes, URL requirements, unversioned names, group includes and
Poetry `git`/`path`/`url` sources are skipped. Poetry constraints use the
`poetry` scheme: `^` keeps the leftmost nonzero component, `~` allows patch
changes (minor for a bare major) and `||` separates alternatives. Names are PEP
503 normalised and resolved from the nearest `uv.lock` or `poetry.lock`.
`.python-version` pins CPython with a numeric version, like mise's `python`.
A release that already satisfies a range is left to lockfile maintenance, as
for Cargo. `[build-system] requires` and `requires-python` are not updated.

Kubernetes workload specs, Kustomize image/helmCharts transforms, Flux
HelmRepository/HelmRelease/OCIRepository and Helm values repository/tag/digest
maps are supported. Flux source references resolve by namespace/name across
selected files. YAML event paths supply exact UTF-8 scalar locations, including
flow collections, quoted values and repeated identical images. ConfigMaps,
Secrets, workload annotations and Compose-as-values are excluded.

Remaining extraction limits: templated/aliased image scalars require concrete
values; Helm templates are not rendered. Cargo git/private registries, Terraform
git/local modules and custom registries, Go replace/exclude/retract semantics,
and pip editable/VCS/URL dependencies are not registry update targets. Lockfile
rewriting and dependency installation remain part of the later run phase.

Independent specifications: [Cargo](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html),
[Dockerfile](https://docs.docker.com/reference/dockerfile/),
[Go modules](https://go.dev/ref/mod#go-mod-file),
[Flux](https://fluxcd.io/flux/components/source/helmrepositories/).

`pre-commit` reads `.pre-commit-config.yaml` (or `.yml`) and updates each hook
repository's `rev`, keeping its YAML quoting. GitHub repositories use
`github-tags`; GitLab and Gitea/Forgejo repositories (gitlab.com, codeberg.org,
gitea.com and hosts labelled `gitlab`, `gitea` or `forgejo`) use `forge-tags`
with the identity `host/path`. `local` and `meta` hooks, other hosts and
40-character commit pins are skipped; commit pins with a frozen tag comment are
not yet supported.

`rust-toolchain` and `rust-toolchain.toml` (Cargo manager) and
`.terraform-version` (Terraform manager) pin their tool with a numeric version,
like mise's `rust` and `terraform`; channels such as `stable`, `nightly-…` or
`latest` are skipped, and these files have no lockfile job.

`helm` reads `Chart.yaml` dependencies from HTTPS and `oci://` repositories
(`file://` charts and repository aliases are local). A committed `Chart.lock`
holds the update as lockfile-pending: `helm dependency update` downloads chart
archives into the tree and is not run yet.

`devcontainer` reads `devcontainer.json` and `.devcontainer.json` (JSONC). The
`image` is a container image, digest-pinned like any other; OCI `features` keep
their version in the feature key (`ghcr.io/devcontainers/features/node:1`), are
edited there and are never digest-pinned. Templated images are skipped.

`maven` reads `pom.xml` with a small span-preserving XML scanner (comments,
CDATA and processing instructions are skipped): `parent`, `dependencies`,
`dependencyManagement`, build `plugins`/`pluginManagement` and `extensions`.
Plugins without a `groupId` use `org.apache.maven.plugins`. A `${property}`
version is edited at its `<properties>` definition; only the first artifact
using a property is emitted, so artifacts sharing it never get conflicting
edits. Profiles and versions inherited from a parent or BOM are not updated.

`gradle` reads version catalogs (`gradle/*.versions.toml`: `[libraries]` and
`[plugins]`, inline `group:artifact:version` strings, module tables and
`version.ref`, edited once in `[versions]` for its first user), and Gradle
scripts (`*.gradle`, `*.gradle.kts`): literal `"group:artifact:version"`
coordinates, `group: …, name: …, version: …` notation and `id "…" version "…"`
plugins, with comments ignored. Under a `src/` directory only `src/main/` is
read (precompiled script plugins); test resources and docs hold fixtures. It
also reads the wrapper's `distributionUrl`. Dynamic (`1.+`) and rich versions
are skipped.

Versions held in variables are resolved and edited at their definition, once,
for their first user. Definitions are `ext { name = "1.0" }`, `ext.name`,
`def`/`val` locals, `extra["name"]`/`ext.set("name", …)`, entries of map
literals (`versions += [jackson: "2.15.0"]` names `versions.jackson`), indexed
maps (`versions["x"] = "1.0"`) and `gradle.properties`. References are
`"g:a:$name"`, `"g:a:${versions.jackson}"`, notation `version: name` and plugin
`version "${name}"`. Kotlin's `"$a.b"` appends `.b` and is not a reference.
Gradle scoping is approximated by one namespace per repository, so a name
defined more than once is skipped, as are computed values (ternaries,
`if`/`else`), the project's own `version`/`group` and Gradle settings
(`org.gradle.*`). A package referenced through several variables keeps
deliberate parallel versions, such as Kafka's compatibility-test pins, and is
skipped.
Plugins resolve through the Gradle Plugin Portal's marker artifacts. When the
wrapper records `distributionSha256Sum`, the lockfile runner replaces it with
the checksum Gradle publishes beside the new distribution, fetched only from
`services.gradle.org`/`downloads.gradle.org`. A committed `gradle.lockfile`
holds updates as lockfile-pending.

Maven versions use Maven's version order (`maven` scheme): case-insensitive
tokens and qualifiers `alpha < beta < milestone < rc < snapshot < release <
sp`. Bare versions are exact requirements and `[a,b)` ranges are kept while
satisfied. A version's release variant (Guava's `-jre`/`-android`, Hibernate's
`.Final`) is kept, so `33.4.8-android` never becomes `33.7.2-jre`.

`nuget` reads `PackageReference`, `PackageVersion` and `GlobalPackageReference`
items (a `Version`/`VersionOverride` attribute or a `<Version>` child) in
`*.csproj`, `*.fsproj`, `*.vbproj`, `Directory.Packages.props` and
`Directory.Build.props`, local tools in `.config/dotnet-tools.json`, and the SDK
in `global.json`. Commented items, `$(Property)` versions and floating `*`
versions are skipped. NuGet versions use the `nuget` scheme: SemVer 2 with an
optional fourth number and case-insensitive labels, bare versions exact and
interval ranges (`[2.9.0,3.0)`) kept while satisfied. The SDK resolves from
published `dotnet/sdk` GitHub releases, accepting only feature-band versions,
because tags can precede a release.

When `packages.lock.json` files exist under an edited file's directory, the
lockfile runner runs `dotnet restore --force-evaluate` on that directory's single
solution (else project) with the `global.json` SDK or the pinned default
(9.0.319), an isolated `NUGET_PACKAGES` and telemetry off, removes the `obj/`
intermediates restore writes, and fails the branch if no lock changed.

`bundler` reads `gem "name", "requirement"` lines and the `ruby` directive in a
`Gemfile`, resolving gems from `Gemfile.lock` specs (platform suffixes
ignored), plus `.ruby-version` (`ruby-` prefix accepted). Gems with `git:`,
`path:` or `github:` sources, gems with two requirement strings and commented
lines are skipped. Requirements use the `rubygems` scheme: Gem version order
(letters make a prerelease, `1.0.0.rc1 < 1.0.0`, four-part versions such as
`8.1.3.1`) and `=`, `!=`, `<`, `<=`, `>`, `>=`, `~>` requirements, rewritten with
their operator and precision (`~> 7.1` becomes `~> 8.0`). The lockfile runner
runs `bundle lock --update <gems>` (all gems for maintenance) with Ruby from
`.ruby-version`, the Gemfile's `ruby` directive or the pinned 3.4.11, a scratch
`GEM_HOME`/`BUNDLE_USER_HOME`, and `BUNDLE_APP_CONFIG` pointed away from any
repository `.bundle/config`. A Gemfile is Ruby: evaluating it runs repository
code inside that scrubbed environment, as Bundler always does.

`composer` reads `require` and `require-dev` in `composer.json`, resolving
packages from `composer.lock` (`packages` and `packages-dev`). Platform
requirements (`php`, `ext-*`, `lib-*`, `composer-plugin-api`,
`composer-runtime-api`), `dev-` branches and `*` are skipped. Constraints use
the `composer` scheme: up to four release numbers with `alpha` < `beta` < `RC`
< stable < `patch`, and `^`, `~` (`~1.2` allows `<2.0`, `~1.2.3` allows `<1.3`),
comparisons, `1.2.*`, hyphen ranges, `,`/space and `||`, rewritten with their
operator and precision (`^6.4` becomes `^7.1`). The lockfile runner runs
`composer update <packages> --with-all-dependencies --minimal-changes`
(everything for maintenance) with `--no-install --no-scripts --no-plugins`, so
no repository code runs and nothing is downloaded beyond metadata. The PHP
version still constrains resolution; extension and library requirements are
ignored because they describe the deployment, not the runner. PHP comes from
the repository's `mise.toml` pin or the pinned 8.5.8, Composer from its pin or
2.10.3, with a scratch `COMPOSER_HOME`. `lockfiles.php` selects the PHP build;
see [configuration](configuration.md).
