# Configuration

`rustovate config check <path>` evaluates Pkl and checks the resulting typed
model. A path can be a file or a repository directory. Discovery checks
`.github/rustovate.pkl` first, then `rustovate.pkl`. The library returns `None`
when neither exists so the future batch runner can skip that repository; an
explicit CLI check reports missing configuration with exit code 2.

Vendor the bundled schema with `rustovate config schema > Rustovate.pkl`, then
write a repository config that `amends "Rustovate.pkl"` (adjust the relative
path if the config is in `.github`). The committed examples amend the schema
at the repository root. `config schema` always prints Pkl, including with
`--json`. `config check --json` returns a versioned validation envelope;
configuration errors exit 2, runtime errors exit 1 and success exits 0.

Organisation policy uses ordinary local Pkl composition:

```pkl
// org.pkl
amends "Rustovate.pkl"
prs { concurrent = 8 }
defaults { releaseNotes = false }
```

```pkl
// rustovate.pkl
amends "org.pkl"
prs { dashboard = true }
```

Local `import` works too. pklr 5.1.1 evaluates everything in process. This
build deliberately enables only local evaluation: HTTP/package imports and
external resource reads (including environment variables) are disabled.
Configuration validation and tests therefore need no network or Pkl executable.
Local modules can read other local modules; this is not a filesystem sandbox.

## Defaults and policy

[The schema](../Rustovate.pkl) is the complete source of defaults. These are
independent choices, not an expansion of a Renovate preset:

- Select all first-release managers. Each manager will select its own supported
  files; `files` entries replace file selection for the named manager.
- Pin exact versions and pin GitHub Actions and container image digests.
- Keep automerge off and dependencies separate. Attach `dependencies`, use
  `chore(deps):` as the commit prefix, and fetch release notes.
- Allow unlimited open PRs and proposals per hour (`null`). Positive values
  limit them; zero is invalid. Keep the dashboard off and use `rustovate/`
  branch names, rebasing when conflicted.
- Do not delay releases, restrict update windows, refresh lockfiles separately,
  process submodules, or ignore any dependencies/authors. Exclude generated
  dependency directories and Git metadata as listed in the schema.

These issues implement policy representation and validation. Extraction,
matching, updating, scheduling, merging, and lockfile execution remain future
phases; accepting a configuration does not perform those operations.

Rules apply in declaration order. Non-null `apply` values replace earlier
values, including whole lists. Null leaves policy unchanged. An empty group
string requests individual PRs; a nonempty group identifies a shared PR.
`digest = false` is an explicit per-dependency opt-out from source-level
integrity defaults, including reusable workflows that must stay on a branch.
`active = false` prevents updates to matched dependencies.

Selector fields combine with AND. Positive patterns in a list combine with OR;
exclusions veto matches, and an all-negative list matches anything not excluded.
An empty list is unrestricted. Patterns are case-sensitive path-aware globs
(`/` is a separator), optionally prefixed `glob:`, or Rust regexes prefixed
`regex:`. Prefix `!` negates a pattern. Version filters must explicitly use
`semver:` (Cargo semver requirement syntax) or `regex:`, with optional `!`.
Package identifiers and dependency identifiers are separate selectors. Manager
names, datasources, update types and version schemes are closed enumerations.
`kinds` is a closed list of exact dependency roles: `production`, `development`,
`peer`, `optional`, `build`, `action`, `reusable-workflow`, `container-image`,
`tool`, `go-directive`, `provider`, `module`, and `chart`. npm/bun distinguish
the first four; Cargo uses production/development/build. Go/Python and default
regex extraction use production; `go-directive` is the minimum Go version.
Actions steps and reusable workflows are distinct. Container managers use
container-image, mise uses tool, Terraform uses provider/module, and Helm uses
chart. Unlike package/file selectors these values do not accept globs or regexes.

Rules distinguish major/minor/patch/digest/pin; both adding a version pin and
adding an integrity digest are classified as pin. Refreshing a lockfile is a
separate operation configured under `lockfiles`, not a version update type.

`waitHours` uses nonnegative whole hours and defaults to 24 for dated releases.
Undated releases have no wait. Set `defaults { waitHours = 0 }` to disable it,
or override it in a matching rule. The gate selects proposals, retaining all
extracted current versions and existing lockfile pins. See [lookup policy](lookup-policy.md).
A window has at least one five-field
cron expression and an IANA timezone. Weekdays use cron numbering (0/7 Sunday,
2 Tuesday). Empty `windows` means unrestricted, including in a rule override.
A window restricts when a proposal is created, not when an existing PR merges.

## Custom extractors

Select files with `paths`, then use one or more Rust regex `patterns`. Every
pattern must capture `current`. The `source`, `dependency`, `package`, and `scheme`
templates can be literals or use `{capture}` placeholders. Every referenced
capture must exist in every pattern. Defaults expect `source` and `dependency`
captures and use the dependency as the registry package identifier. Static
sources and schemes must be known names; dynamic values will be checked at
extraction time. `scheme` selects version comparison. `extract` optionally
normalizes registry tags using a required named `version` capture.

Patterns use Rust regex syntax (no lookaround or backreferences). Pkl raw
strings, such as `#"(?<current>\S+)"#`, avoid double escaping. Existing comments
in repository files may keep their `renovate:` marker: it is just literal text
in a repository-owned file. No Renovate parser or fixture is involved.

Templates substitute named values only, without arbitrary expressions or
helpers. PR `bodyNotes` reserves `{releaseNotes}` for the future renderer.
`defaults.releaseNotes = false` turns release-note fetching off.

## Diagnostics and examples

Every Rust config struct denies unknown fields. Pkl catches type violations;
Rust additionally validates enum values, regexes, globs, templates, version
filters, cron/timezone values, branch prefixes and limits. Diagnostics include
the config path and logical field. pklr syntax errors retain exact source
spans. Rust validation highlights an unambiguous property assignment when it
exists in the checked file. Evaluated/inherited values do not have a reliable
source map; those diagnostics report the field without inventing a location.

The twelve [examples](../examples) were written from the repositories' own
configs fetched on 2026-10-14, plus `jdx/renovate-config/default.json` for fnox.
fnox's source is `.github/renovate.json`; the other eleven are `renovate.json`
at the repository root. Opaque preset names were not resolved or copied.
Examples preserve explicit policies and use the documented rustovate defaults
where a source only names a preset. Consequently they intentionally do not
claim identical behavior for unexamined presets.

The fnox example preserves the organisation's explicit automerge setting
(including majors), weekly window, release age, jdx exceptions, grouping,
submodules, ignored authors and paths, and lockfile policies. The source's
abandonment preset is opaque and is not expanded. Config migration and
abandonment heuristics have no corresponding behavior here. The renovate-ce
example explicitly enables `regex` so its intended custom extractor is active.
The remote-agent example keeps nightly updates separate and manually merged.
The immich-forge-cli example retains its explicit `renovate/` branch prefix.

With pklr 5.1.1, use explicit class constructors inside listings: `new Rule`,
`new Extractor`, `new FileSet` and `new Window`. Anonymous `new { ... }` entries
do not consistently receive class defaults from an enclosing `Listing<T>`.
The examples use explicit constructors and the tests cover their defaults.
