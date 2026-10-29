---
title: "Configuration"
---
The Count reads [Pkl](https://pkl-lang.org), evaluated in process. A repository's
config amends the `PatchulaConfig.pkl` schema, and every field it leaves
alone keeps the schema's default. There is no JSON reader, no preset system and
no migration tool. If you are coming from Renovate, see [migrating a
config](/renovate/migrating/).

## Where the config lives

The Count reads `patchula.pkl` at the repository root. A repository without
one is skipped, so nothing happens to a repository that did not ask. That holds
for `run` and `lookup` on named repositories, `--all-repos` and the server.

The working tree is different, because the person running the command is the
one asking. `lookup`, `tui` and `run --dry-run` with no repository named, in a
checkout with no `patchula.pkl`, ask at a terminal whether to use the defaults
for this run, write a starter `patchula.pkl`, or quit. The starter file is
only `amends "PatchulaConfig.pkl"` and a comment. Without a terminal they fail
and name the two ways on: `--default-config` for the defaults this run, or a
committed `patchula.pkl`.

```pkl
// patchula.pkl
amends "PatchulaConfig.pkl"

prs {
  dashboard = true
  concurrent = 10
}
defaults {
  labels { "dependencies"; "bot" }
}
```

The schema does not have to be committed. When the Count evaluates a config he
supplies `PatchulaConfig.pkl` himself. Vendor it for completion in an editor;
a vendored copy is the one that gets evaluated, so keep it current with the
Count you run:

```sh
patchula config schema > PatchulaConfig.pkl
patchula config check .            # a file or a repository directory
patchula config check --json .     # a versioned validation envelope
```

Configuration errors exit 2; the [CLI reference](/reference/cli/) lists every exit code.

## Sharing policy across repositories

Organisation policy is ordinary Pkl composition: one module amends the schema,
and repositories amend that module.

```pkl
// org.pkl
amends "PatchulaConfig.pkl"

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

The [schema](/reference/configuration/) is the complete list. In short, the Count
starts out cautious:

- every supported manager is selected, each with its own file patterns;
- versions are pinned exactly, and GitHub Actions and container images are
  pinned to digests;
- every update gets its own PR with the `dependencies` label and a
  `chore(deps):` commit prefix, and release notes are included;
- nothing auto-merges, except [reference data](#reference-data) such as CA
  root bundles, which also skips the release-age wait;
- dated releases wait 24 hours before they are proposed;
- there is no limit on open PRs or PRs per hour, no schedule window and no
  dashboard;
- branches start with `patchula/` and are rebased only when they conflict;
- installed and vendored dependencies (`node_modules`, `bower_components`,
  `vendor`), `.git`, and test and example directories (`test`, `tests`,
  `__tests__`, `__fixtures__`, `fixtures`, `examples`) are excluded, at any
  depth.

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
| `kinds` | dependency [kinds](/guide/managers/#kinds), such as `production`, `action` or `tool` |
| `current` | the current version, through `semver:` or `regex:` |

Patterns are case-sensitive, path-aware globs (`/` is a separator), optionally
written `glob:...`, or Rust regexes written `regex:...`. A leading `!` excludes.
A list made only of exclusions matches everything they do not exclude. Version
filters must say which syntax they use: `current = "semver:<2"` or
`current = "regex:^0\\."`. Manager, source, update and kind values are closed
lists, so a typo is an error.

Both adding a version pin and adding a digest count as `pin`. Refreshing a
lockfile is not an update type; it is configured under
[`lockfiles`](/guide/lockfiles/#lockfile-maintenance).

### What a rule can change

| Field | Effect |
|---|---|
| `active` | `false` stops updates for the selected dependencies |
| `versions` | `exact` pins, `preserve` keeps and shifts ranges, `widen` extends ranges to include the new version |
| `channel` | `stable`, or `lts` for Node's long-term support line |
| `digest` | `false` opts out of digest pinning |
| `merge` | allow [auto-merge](/guide/auto-merge/) |
| `group` | updates with the same group share a PR; `""` forces separate PRs |
| `labels`, `commitPrefix` | PR labels and commit message prefix |
| `releaseNotes`, `bodyNotes` | release notes in the PR body, and extra lines of your own |
| `waitHours` | release-age wait in whole hours; `0` turns it off |
| `windows` | when proposals may be created |
| `permitted` | a version filter targets must match, such as `"semver:^24"`; an explicit filter also allows prereleases |

Windows are five-field cron expressions in an IANA timezone; weekdays count
0 or 7 for Sunday and 1 for Monday. Each time an expression names opens a turn:

- The first run after that time proposes and updates PRs.
- Later runs hold them until the next time comes round, so `"0 3 * * *"` means
  once a day, after 03:00, however often the Count runs (server sweeps,
  webhooks or a CI schedule).
- An expression that covers a range, such as every minute of a morning, lets
  every run in that range propose.

Windows decide when updates are proposed, not when they merge: a PR whose
checks pass later still merges. A repository's first run, before the Count has
recorded one, proposes only if it falls on one of the window's minutes.

```pkl
defaults {
  windows {
    new Window {
      cron { "0 3 * * 1" }   // once a week, after 03:00 on Mondays
      zone = "Europe/London"
    }
  }
}
```

## Reference data

Some packages ship data rather than code: CA root bundles, the time zone
database and the public suffix list. They only help while they are current.
When a certificate authority is distrusted, a client that bundles its own roots
keeps trusting it until that package is updated.

So these packages also have the kind `reference-data`, and the schema's first
rule fast-tracks them: no release-age wait, no schedule window, a PR of their
own, and [auto-merge](/guide/auto-merge/) once checks pass. Failing checks,
required reviews and branch protection still stop the merge.

| Ecosystem | Packages |
|---|---|
| Python | `certifi`, `tzdata`, `pytz`, `publicsuffixlist` |
| Rust | `webpki-roots`, `webpki-root-certs`, `chrono-tz`, `psl` |
| Go | `golang.org/x/crypto/x509roots/fallback` |
| npm | `psl`, `tldts` |
| Ruby | `tzinfo-data`, `public_suffix` |
| PHP | `composer/ca-bundle` |

The built-in rule comes first, so a rule of your own can change it:

```pkl
rules {
  new Rule {
    note = "Review root bundle updates by hand."
    select { kinds { "reference-data" } }
    apply { merge = false; waitHours = 24 }
  }
}
```

:::caution
Assigning `rules = new Listing { ... }` replaces the whole list, the built-in
rule included.
:::

Root certificates inside container images come from the image's
operating system packages; the Count only sees the image's digest change, so
those are not covered.

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

:::tip
Limits are positive integers or `null`. Zero is rejected, because zero meaning
"unlimited" is a trap.
:::

See [branches and PRs](/guide/pull-requests/),
[batching](/guide/batch/) and [lockfiles](/guide/lockfiles/).

## Choosing files and managers

`managers` lists the active managers and `exclude` the excluded paths. Both
start out full, so assign a `new Listing` to replace them: amending with
`managers { ... }` adds to the defaults instead. `files` replaces one manager's
file selection. `ignore` lists dependency names to leave alone, with the same
pattern syntax.

```pkl
managers = new Listing { "npm"; "github-actions"; "mise" }
exclude { "generated/**" }
files {
  new FileSet {
    manager = "dockerfile"
    paths { "docker/*.Dockerfile" }
  }
}
ignore { "glob:@internal/*" }
```

To read a directory the defaults exclude, such as a real app under
`examples/`, amend `exclude` with a `!` entry. It keeps every default and
re-includes only the paths it matches:

```pkl
exclude { "!examples/app/**" }
```

Assigning `exclude = new Listing { ... }` drops the defaults, so list any you
still want.

`useVersionsHost = false` skips the public
[mise-versions](/guide/sources/#mise-versions-host) mirror, for GitHub releases and
tags and for the mise lookups that mise runs, and reads every catalog from its
upstream.

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
`#"(?<current>\S+)"#` avoid double escaping.

## Diagnostics

Every field is checked. Pkl catches type errors; the Count then validates enum
values, regexes, globs, templates, version filters, cron expressions,
timezones, branch prefixes and limits. Errors name the file and field, and
point at the exact assignment when it is in the checked file.

:::caution[One pklr quirk]
Inside listings, write the class explicitly (`new Rule`,
`new Extractor`, `new FileSet`, `new Window`). An anonymous `new { ... }` does
not always pick up the class's defaults.
:::

## Examples

The [examples directory](https://github.com/sweepies/count-patchula/tree/main/examples)
holds configs translated from real repositories' Renovate setups, written by
hand from their own `renovate.json` files. Opaque preset names were not
expanded, so they show the Count's defaults wherever a source only named a
preset.
