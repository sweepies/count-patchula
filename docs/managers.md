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
not registry dependencies. bun.lock JSONC and npm lock versions 1–3 provide
resolved versions. Binary bun.lockb and pnpm/yarn locks remain unsupported.

Workflow extraction follows jobs, steps, reusable workflows, services and
composite action steps, ignoring run text, expressions and local refs. SHA pins
expose an optional version comment as their current value and the SHA as their
digest. Images retain tags/digests but Docker lookup
is not implemented. Flow-style and YAML alias scalar locations currently fail
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
