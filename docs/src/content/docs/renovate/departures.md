---
title: "Departures from Renovate"
---
The Count is an independent project, not a Renovate clone, and he does many
things his own way. This page lists only the differences a Renovate user is
likely to trip over: defaults that differ, features they would expect that are
missing, and behaviour that changes their pull requests. The
[compatibility matrix](/renovate/compatibility/) maps managers and options, and
[migrating a config](/renovate/migrating/) translates a `renovate.json`.

## Configuration

### Pkl, not JSON {#pkl-configuration}

- **Renovate:** JSON or JSON5, with named presets, `local>` references and config migration
- **The Count:** Pkl only, with a typed schema; unknown fields are errors. Configs are shared with ordinary Pkl `amends` and `import` of local files, with no remote imports
- **Why:** Typed, composable config that is validated when it loads, without network access

### No presets; explicit defaults {#explicit-defaults}

- **Renovate:** Most of a typical config comes from presets such as `config:recommended`
- **The Count:** No presets. The schema's defaults are exact versions (ranges kept for Cargo, pyproject, Bundler and Composer, where a lockfile holds the version), digest pins for actions and images, manual merges, one PR per update, release notes, a dependency label and a semantic commit prefix
- **Why:** Every default is visible in the schema, so a config says exactly what it changes

### Default excluded paths {#default-exclude}

- **Renovate:** `ignorePaths` defaults to `node_modules` and `bower_components`; `vendor`, test and example directories are ignored only by the `config:recommended` preset. A repository's `ignorePaths` replaces the list
- **The Count:** `exclude` defaults to `node_modules`, `bower_components`, `vendor`, `.git`, `test`, `tests`, `__tests__`, `__fixtures__`, `fixtures` and `examples` at any depth. Amending adds to the defaults and a `!` entry re-includes a path; assigning a new Listing replaces them
- **Why:** With no preset layer, the defaults carry what a recommended setup ignores; test data and examples hold manifests nobody deploys

### Prefixed patterns {#match-syntax}

- **Renovate:** Match fields accept either a slash-delimited regex or a glob
- **The Count:** Patterns need an explicit `regex:` or `glob:` prefix, optionally negated with `!`; globs are path-aware
- **Why:** No guessing from delimiters, and literal slashes in package names stay literal

### Rules replace lists {#rules}

- **Renovate:** `packageRules` merge, so a later rule adds to an earlier rule's lists
- **The Count:** `rules` apply in order and a later rule replaces whole values, lists included. An empty `group` means separate PRs
- **Why:** A smaller, explicit policy model: the last rule that sets a value wins

### One extractor model {#custom-extractors}

- **Renovate:** Regex custom managers with `currentValue` and `depName` captures and Handlebars templates
- **The Count:** Pkl `extractors` with `current`, `source` and `dependency` captures and plain `{capture}` substitution, in Rust regex syntax, which has no lookaround
- **Why:** One validated model with no executable template helpers

### Schedules are cron windows {#release-timing}

- **Renovate:** Natural-language or cron `schedule` with a `timezone`, and duration strings
- **The Count:** `waitHours` plus cron `windows` with a named time zone; a window is due on the first run after it opens. Empty windows mean any time
- **Why:** A mistyped schedule is an error when the config loads, instead of a window that never opens

### Zero is not unlimited {#limits}

- **Renovate:** `0` means unlimited for limits such as `prConcurrentLimit`
- **The Count:** Limits are unlimited by default, `null` lifts one, and zero is rejected
- **Why:** Zero meaning "unlimited" is easy to set by accident

### Branch and commit names {#branch-names}

- **Renovate:** `renovate/` branch prefix; `semanticCommits` switches the commit style
- **The Count:** `patchula/` branch prefix and a `chore(deps):` commit prefix (`fix(deps):` for security fixes, so release tooling ships them), both overridable
- **Why:** His own identity and predictable messages

## How the Count runs

### No onboarding pull requests {#no-onboarding}

- **Renovate:** Opens an onboarding PR in a repository without a config
- **The Count:** Skips repositories without a config. In a local checkout with no config, the CLI offers its defaults or a starter `patchula.pkl` at a terminal, and `--default-config` uses the defaults without asking
- **Why:** Repositories opt in deliberately

## What gets updated

### Indirect Go requirements {#indirect-go-requirements}

- **Renovate:** Indirect requirements are off by default
- **The Count:** Every explicit `go.mod` requirement is maintained, indirect ones included
- **Why:** A security fix should not wait for a direct dependency to move

### mise tools {#mise-backends}

- **Renovate:** Looks each tool up through its backend's datasource, under the backend's package name
- **The Count:** Asks the pinned mise to list each tool (`mise ls-remote --json --prerelease`) in a sandbox, and reports the tool's own name as its package with the source `mise`
- **Why:** Every backend mise can list works, with mise's own registry and version rules

Rules and branch names use the tool's name: `jq`, not `jqlang/jq`, and `patchula/jq-1.x`. `asdf:` and `vfox:` tools, and registry names that only have a plugin backend, are refused with a warning, because their plugins are code from the repository being read. See [mise lookups](/guide/sources/#mise-lookups).

### Nix flake inputs {#nix-flake-inputs}

- **Renovate:** The `nix` manager is beta and off by default
- **The Count:** `nix` is on by default; it reads `flake.lock` beside a `flake.nix` and resolves each input with `nix flake prefetch`
- **Why:** Locking runs no repository code, so there is no reason to keep it opt-in

See [Nix](/guide/managers/#nix).

### Copier templates are reported, not applied {#copier-templates}

- **Renovate:** Runs `copier update` to move a template to its new tag, with `--trust` when allowed
- **The Count:** Reports the new tag and opens no PR. Copier runs only on your own working tree, with your own `copier`, when you ask for it in the TUI or with `patchula apply --copier`, and never with `--trust`
- **Why:** Applying a template runs the template author's code at a new, unreviewed version; on the server that code would run next to the installation token and other repositories

The Count never bumps `_commit` on its own either, since the next `copier update` would diff from the wrong base. See [Copier](/guide/managers/#copier).

### A 24-hour release-age wait {#minimum-release-age}

- **Renovate:** `minimumReleaseAge` is off by default
- **The Count:** `waitHours` is 24 by default for dated releases; the newest eligible release is proposed and newer ones are reported as held back
- **Why:** It follows mise's default `minimum_release_age`; undated releases have no wait

See [the release-age wait](/guide/lookup-policy/#the-release-age-wait).

### Exact npm versions {#npm-range-edits}

- **Renovate:** `rangeStrategy` defaults to `auto`, which keeps a caret or tilde range and updates only the lockfile when the new version satisfies it
- **The Count:** `versions = "exact"` pins dependencies and devDependencies; peer ranges are widened, never pinned
- **Why:** Reproducible installs, while consumers keep the peer ranges they support

See [version policies](/guide/versioning/#version-policies).

### A separate patch track {#patch-track}

- **Renovate:** Proposes the highest non-major release
- **The Count:** Offers a patch update alongside the minor one
- **Why:** The smaller upgrade can be taken on its own

### Stable releases, not LTS {#release-channels}

- **Renovate:** Node targets follow the LTS line
- **The Count:** Proposes the newest eligible stable version the policy allows
- **Why:** A hidden LTS restriction should not be inferred from a package name

For an opt-in Node LTS rule, see [lookup policy](/guide/lookup-policy/#node-release-channels).

### Warnings when auto-merge has nothing behind it {#tooling-warnings}

- **Renovate:** Automerges as configured, whatever CI the repository has
- **The Count:** Warns on PRs set to merge, and on the dashboard, only on positive evidence: no CI runs on the PR and no checks reported on recent default-branch commits; GitHub's native auto-merge is on and no status check is required; or a security fix merges, nothing runs on the default branch for its files and no checks reported on recent commits. What he can't read is unknown and never warns
- **Why:** An update that merges untested, or merges and never ships, is worse than one left open

See [tooling warnings](/guide/auto-merge/#tooling-warnings).

### Reference data merges itself {#reference-data}

- **Renovate:** No built-in category; release age, schedules and automerge apply as configured
- **The Count:** CA root bundles, time zone data and the public suffix list are `reference-data`; by default their updates skip the wait and windows, get their own PR and merge once checks pass
- **Why:** A stale trust store keeps trusting a distrusted CA, and these packages change data, not APIs

See [reference data](/guide/configuration/#reference-data).

### Advisories from OSV.dev {#advisories}

- **Renovate:** `vulnerabilityAlerts` reads the repository's Dependabot alerts and opens separate alert PRs
- **The Count:** OSV.dev only, on by default; updates that fix an advisory are marked and labelled, with no separate alert PRs
- **Why:** Works without Dependabot or the App's `vulnerability_alerts` permission, and fits one batched query per run

See [security advisories](/guide/security/).

### Image rebuild advisories {#image-advisories}

- **Renovate:** Vulnerability alerts cover package ecosystems, not the OS packages inside an image
- **The Count:** Compares a pinned image's old and new SBOMs through OSV.dev; a rebuild whose new digest no longer has an advisory the old digest had is a security update
- **Why:** Rebuilds of a pinned tag are mostly OS security fixes, which otherwise look like any other digest change

See [container image rebuilds](/guide/security/#container-image-rebuilds).

## Lockfiles

### aube for npm and pnpm {#node-lockfile-engine}

- **Renovate:** Regenerates npm and pnpm locks with npm and pnpm
- **The Count:** Regenerates `aube-lock.yaml`, `pnpm-lock.yaml`, `package-lock.json` and `npm-shrinkwrap.json` with aube; `lockfiles.npmEngine = "npm"` restores native npm for npm lockfiles
- **Why:** Lockfile updates are 10 to 20 times faster; [why aube](/guide/lockfiles/#why-aube-for-npm-and-pnpm) lists how its output differs from npm's
