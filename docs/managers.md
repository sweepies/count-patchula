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
retain an optional version comment. Images retain tags/digests but Docker lookup
is not implemented. Flow-style and YAML alias scalar locations currently fail
explicitly; normal block YAML is supported. Regex extractors use the Pkl model.

mise supports strings, arrays, version tables and .tool-versions. Common core
names have explicit GitHub identities; github:/aqua:/npm: backends are recognized.
Unknown aliases retain the mise source and cause a lookup warning, rather than
being guessed. Runtime options in tables are not dependencies.

References used for independent tests: [package.json](https://docs.npmjs.com/cli/v11/configuring-npm/package-json),
[Bun lockfile](https://bun.sh/docs/pm/lockfile),
[workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax),
[mise configuration](https://mise.jdx.dev/configuration.html).
