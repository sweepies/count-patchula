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
