# Configuration

The Count reads [Pkl](https://pkl-lang.org), evaluated in process. A repository's
config amends the published `Patchula.pkl` schema, and every field it leaves
alone keeps the schema's default. There is no JSON reader, no preset system and
no migration tool. If you are coming from Renovate, see [migrating a
config](/renovate/migrating).

## Where the config lives

The Count looks for `.github/patchula.pkl` first, then `patchula.pkl` at the
repository root. A repository with neither is skipped, so nothing happens to a
repository that did not ask.

```pkl
// .github/patchula.pkl
amends "../Patchula.pkl"

prs {
  dashboard = true
  concurrent = 10
}
defaults {
  labels { "dependencies"; "bot" }
}
```

The schema does not have to be committed. When the Count evaluates a
repository's config he provides `Patchula.pkl` himself. Vendor it to check a
config locally or to get completion in an editor:

```sh
patchula config schema > Patchula.pkl
patchula config check .            # a file or a repository directory
patchula config check --json .     # a versioned validation envelope
```

Configuration errors exit 2, runtime errors exit 1.

## Sharing policy across repositories

Organisation policy is ordinary Pkl composition: one module amends the schema,
and repositories amend that module.

```pkl
// org.pkl
amends "Patchula.pkl"

prs { concurrent = 8 }
defaults { releaseNotes = false }
```

```pkl
// patchula.pkl
amends "org.pkl"

prs { dashboard = true }
```

Local `import` works too. Remote imports, package imports, environment
variables and other external reads are disabled, so validation needs no
network and no Pkl executable, and two machines always agree on what a config
means.

## Defaults

The [schema](/reference/configuration) is the complete list. In short, the Count
starts out cautious:

- every supported manager is selected, each with its own file patterns;
- versions are pinned exactly, and GitHub Actions and container images are
  pinned to digests;
- every update gets its own PR with the `dependencies` label and a
  `chore(deps):` commit prefix, and release notes are included;
- nothing auto-merges;
- dated releases wait 24 hours before they are proposed;
- there is no limit on open PRs or PRs per hour, no schedule window and no
  dashboard;
- branches start with `patchula/` and are rebased only when they conflict;
- `node_modules`, `vendor` and `.git` directories are excluded.

## Rules

Rules adjust policy for the dependencies they select. They apply in order, and
a later rule's values replace an earlier one's, whole lists included. A field
left at `null` changes nothing.

```pkl
rules {
  new Rule {
    note = "Patch updates merge themselves once checks pass."
    select { updates { "patch" } }
    apply { merge = true }
  }
  new Rule {
    note = "One PR for the eslint family."
    select { packages { "eslint"; "glob:@eslint/*"; "glob:eslint-plugin-*" } }
    apply { group = "eslint" }
  }
  new Rule {
    note = "Leave the Kafka compatibility pins alone."
    select { files { "glob:tests/compat/**" } }
    apply { active = false }
  }
}
```

A selector's fields combine with AND. Within one list, patterns combine with
OR. An empty list means "any".

| Selector | Matches |
|---|---|
| `packages` | registry identities, such as `@types/node` or `com.google.guava:guava` |
| `dependencies` | names as written in the file |
| `managers` | manager names, such as `npm` or `github-actions` |
| `files` | paths in the repository |
| `updates` | `major`, `minor`, `patch`, `digest` or `pin` |
| `sources` | sources, such as `npm` or `docker` |
| `kinds` | dependency roles: `production`, `development`, `peer`, `optional`, `build`, `action`, `reusable-workflow`, `runner`, `container-image`, `tool`, `go-directive`, `toolchain`, `provider`, `module`, `chart` |
| `current` | the current version, through `semver:` or `regex:` |

Patterns are case-sensitive, path-aware globs (`/` is a separator), optionally
written `glob:...`, or Rust regexes written `regex:...`. A leading `!` excludes.
A list made only of exclusions matches everything they do not exclude. Version
filters must say which syntax they use: `current = "semver:<2"` or
`current = "regex:^0\\."`. Manager, source, update and kind values are closed
lists, so a typo is an error.

Both adding a version pin and adding a digest count as `pin`. Refreshing a
lockfile is not an update type; it is configured under
[`lockfiles`](/guide/lockfiles#lockfile-maintenance).

### What a rule can change

| Field | Effect |
|---|---|
| `active` | `false` stops updates for the selected dependencies |
| `versions` | `exact` pins, `preserve` keeps and shifts ranges, `widen` extends ranges to include the new version |
| `channel` | `stable`, or `lts` for Node's long-term support line |
| `digest` | `false` opts out of digest pinning |
| `merge` | allow [auto-merge](/guide/auto-merge) |
| `group` | updates with the same group share a PR; `""` forces separate PRs |
| `labels`, `commitPrefix` | PR labels and commit message prefix |
| `releaseNotes`, `bodyNotes` | release notes in the PR body, and extra lines of your own |
| `waitHours` | release-age wait in whole hours; `0` turns it off |
| `windows` | when proposals may be created |
| `permitted` | a version filter targets must match, such as `"semver:^24"`; an explicit filter also allows prereleases |

Windows are cron expressions in an IANA timezone. A window restricts when an
update is proposed, not when an open PR may merge.

```pkl
defaults {
  windows {
    new Window {
      cron { "* 0-6 * * 1" }   // Mondays before 07:00
      zone = "Europe/London"
    }
  }
}
```

## Pull requests and lockfiles

```pkl
prs {
  concurrent = 10        // open PRs at once; null is unlimited
  perHour = 4            // new PRs per hour; null is unlimited
  dashboard = true       // keep a Dependency Dashboard issue
  branch = "patchula/"   // branch prefix
  rebase = "behind"      // rebase whenever the base moves, not only on conflict
  batchMerge = false     // let the batch PR auto-merge
}
lockfiles {
  refresh = true         // weekly lockfile maintenance PR
  tidyGo = true          // go mod tidy after Go updates
  npmEngine = "aube"     // or "npm" for package-lock.json
  php = "static"         // or "compiled"
}
```

Limits are positive integers or `null`. Zero is rejected, because zero meaning
"unlimited" is a trap. See [branches and PRs](/guide/pull-requests),
[batching](/guide/batch) and [lockfiles](/guide/lockfiles).

## Choosing files and managers

`managers` replaces the list of active managers. `files` replaces one
manager's file selection. `exclude` replaces the excluded paths. `ignore`
lists dependency names to leave alone, with the same pattern syntax.

```pkl
managers { "npm"; "github-actions"; "mise" }
files {
  new FileSet {
    manager = "dockerfile"
    paths { "docker/*.Dockerfile" }
  }
}
ignore { "glob:@internal/*" }
```

`useVersionsHost = false` skips the public
[mise-versions](/guide/sources#mise-versions-host) mirror and reads every catalog
from its upstream.

`submodules` and `ignoreAuthors` are accepted and validated, but the Count does
not act on them yet: submodules are not processed, and every push to the
default branch is treated alike.

## Custom extractors

The `regex` manager finds versions the built-in managers do not know about.
Select files with `paths` and give one or more Rust regexes. Each pattern must
capture `current`.

```pkl
extractors {
  new Extractor {
    note = "Versions pinned in a Makefile."
    paths { "Makefile" }
    patterns { #"(?<dependency>[a-z0-9-]+/[a-z0-9-]+)@v(?<current>\d+\.\d+\.\d+)"# }
    source = "github-releases"
    extract = #"^v?(?<version>.+)$"#
  }
}
```

`source`, `dependency`, `package` and `scheme` are templates. They can be
literals or substitute captures with `{capture}`; by default they expect
`source` and `dependency` captures, and the dependency doubles as the package
identity. `scheme` picks the version comparison. `extract` normalises registry
tags and needs a named `version` capture.

Rust regexes have no lookaround or backreferences. Pkl raw strings such as
`#"(?<current>\S+)"#` avoid double escaping. A `renovate:` comment marker in
your files is fine: it is just text in a file you own.

## Diagnostics

Every field is checked. Pkl catches type errors; the Count then validates enum
values, regexes, globs, templates, version filters, cron expressions,
timezones, branch prefixes and limits. Errors name the file and field, and
point at the exact assignment when it is in the checked file.

One pklr quirk: inside listings, write the class explicitly (`new Rule`,
`new Extractor`, `new FileSet`, `new Window`). An anonymous `new { ... }` does
not always pick up the class's defaults.

## Examples

The [examples directory](https://github.com/sweepies/count-patchula/tree/main/examples)
holds configs translated from real repositories' Renovate setups, written by
hand from their own `renovate.json` files. Opaque preset names were not
expanded, so they show the Count's defaults wherever a source only named a
preset.
