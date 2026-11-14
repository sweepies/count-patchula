---
title: "Lockfiles"
description: "How the Count regenerates lockfiles with each ecosystem's real tool, which tools cover which lockfiles, and what happens when one fails."
---
An update that leaves a stale lockfile behind is not an update. When a changed
manifest has a committed lockfile, the Count regenerates it with the
ecosystem's real tool, and the manifest and lockfile go into the same verified
commit. If the tool fails, nothing is published.

## Tools by lockfile

| Lockfile | Tool | Command |
|---|---|---|
| `aube-lock.yaml`, `pnpm-lock.yaml` | aube | `aube install --lockfile-only --ignore-scripts`, once per lockfile, with aube's `default-lockfile` set to that file's name |
| `package-lock.json`, `npm-shrinkwrap.json` | aube, or npm | `npm install --package-lock-only --ignore-scripts` |
| `yarn.lock` (Classic, v1) | aube | `aube install --lockfile-only --ignore-scripts`, with `default-lockfile` set to `yarn.lock` |
| `yarn.lock` (Berry) | Yarn | `yarn install --mode=update-lockfile` |
| `bun.lock` | Bun | `bun install --lockfile-only --ignore-scripts` |
| `Cargo.lock` | Cargo | `cargo update -p <crate>@<old> --precise <target>` |
| `go.sum` | Go | `go get <module>@<target>`, then `go mod download` or `go mod tidy` |
| `.terraform.lock.hcl` | Terraform | `terraform providers lock` |
| `uv.lock` | uv | `uv lock --upgrade-package <package>`, or `<package>==<target>` for a [lockfile-only update](#in-range-updates) |
| `poetry.lock` | Poetry | `poetry lock --no-plugins` |
| `packages.lock.json` | dotnet | `dotnet restore --force-evaluate` |
| `Gemfile.lock` | Bundler | `bundle lock --update <gems>` |
| `composer.lock` | Composer | `composer update <packages> --with-all-dependencies --minimal-changes --no-install --no-scripts --no-plugins`, plus `--with <package>:<target>` for a lockfile-only update |
| `mise.lock` | mise | `mise lock` with the lockfile's recorded platforms |
| `flake.lock` | nix | `nix flake update <input>`, or `nix flake update` for maintenance |
| `apm.lock.yaml` | APM | `apm lock`, or `apm lock --update` for maintenance |
| `.github/workflows/actions.lock` | gh-actions-lock | `gh-actions-lock --no-interactive --no-onboard --no-narrow --ignore-local-actions --json=valid,findings <workflows>`, or the same with `--relock` and no workflows for maintenance |

A `mise.lock` records the configuration in its own directory and above it,
as mise does. A `mise.toml` in a subdirectory is locked by a `mise.lock` of its
own, and with none beside it the Count updates the manifest alone. Tools with no
platform entries, such as `rust`, lock for the platforms the rest of the
lockfile records.

Tools a task installs for itself are locked with the rest: those under
`[tasks.<name>]` in `mise.toml` or `.mise.toml`, and those in a TOML task file the
config includes with `task_config.includes`. The `mise.lock` beside the config
that includes a task file locks it, wherever the task file is. A tool declared
with different versions is locked at each. mise locks the same version with
different options as a separate entry, which the Count cannot write, so that
update fails with a message saying so.

A Cargo update across a semver-incompatible boundary, such as `0.22` to `0.23`
or `1.x` to `2.x`, adds a second entry beside the old one, because other crates
may still require it. The Count runs `cargo update --workspace` for it, which
locks what the edited manifests need and moves no other package, then pins the
entry to the target with `--precise` if Cargo locked a newer release.

Install scripts are skipped (see [Isolation](#isolation) for the code that
still runs). A lockfile that does not change after an edit that
changed recorded dependencies fails the branch, so a tool that says "up to date"
without re-resolving cannot publish a stale lock. An edit that cannot affect the
lock, such as a description, is not a failure. Where the Count can read the
lock back (npm, `aube-lock.yaml`, Bun, Cargo, uv, Poetry, Bundler, Composer,
Terraform providers), the requested version must be the one the new lock
records. `pnpm-lock.yaml` and `yarn.lock` are not read back.

:::caution[Not supported]
Binary `bun.lockb` is not supported, and neither is regenerating `Chart.lock`,
`gradle.lockfile` or the Python locks `pdm.lock`, `pylock.toml` and
`Pipfile.lock`. An update to a manifest with one of those beside it holds as
lockfile-pending, with the lockfile named in the reason, rather than opening a
PR whose lock disagrees with the manifest.
:::

aube keeps its own `aube-lock.yaml`, in pnpm's v9 format, beside the manifest.
The Count regenerates a committed one in place. When it sits beside another
lockfile he regenerates both, each by name, and never lets aube start a new
one. Where aube reads several, `aube-lock.yaml` wins over `pnpm-lock.yaml`.

## In-range updates {#in-range-updates}

A release that a manifest's range already allows needs no manifest edit. For
Cargo, `pyproject.toml` (uv and Poetry), Bundler and Composer the default
[version policy](/guide/versioning/#version-policies) is `preserve`, so such a
release becomes a lockfile-only update: the PR changes the lockfile and no
manifest, and moves that package to the chosen version. The lock may also
contain the transitive changes the new version needs, which Composer resolves
with `--with-all-dependencies --minimal-changes`.

- Cargo, uv and Composer are told the exact version on the command line.
- Poetry and Bundler cannot be. For those the Count pins the declaration to the
  target for one resolution, restores the manifest, and resolves again, which
  keeps the locked version and writes the original constraint back into the
  lock. The manifest in the PR is untouched.
- The version the update starts from is the one the lockfile records for the
  manifest's own dependency (for Cargo, the crates.io edge of the manifest's
  package, or of every member for a workspace root; not another version of the
  same crate locked for a dependent, a path dependency outside the workspace or
  a git dependency), and the
  new lock must record the target or nothing is published. A workspace root's
  members are read from the manifests in the scan. A virtual root (a
  `[workspace]` with no `[package]`) with none of them there has no locked
  version for its dependencies and gets no lock-only update; a root with its own
  `[package]` always counts that package's dependencies, members or not.
- Without a committed lockfile there is nothing to change, so the release is
  not an update. npm and Bun have no such command, so they only get this when
  the scheduled refresh below runs.

## Why aube for npm, pnpm and Yarn Classic

[aube](https://aube.sh) rewrites pnpm and npm lockfiles in place about 10 to 20
times faster than pnpm or npm (0.1 s against 1.5 to 2.5 s on a 265-package
project), and Yarn Classic locks about 30 times faster than Yarn 1 (25 ms
against 0.8 s on a 70-package project), so the Count uses it for all three.

aube's npm lockfiles stay valid for `npm ci`,
but a later `npm install` may rewrite a few lines (hoisting and `funding`
metadata). On larger pnpm graphs aube may keep a second copy of a package pnpm
would deduplicate. `lockfiles.npmEngine = "npm"` restores native npm for
`package-lock.json`. Native npm is also used automatically for format v1 locks,
which aube cannot read. Trees with local `file:` dependencies stay with aube,
which keeps them valid since 2.8.1
([aube#1796](https://github.com/aubepkg/aube/issues/1796)).

aube's Yarn Classic output matches Yarn 1's for the same update, and an
unchanged lock stays byte for byte the same
([aube#1885](https://github.com/aubepkg/aube/issues/1885), fixed in 2.9.0). A
few newly locked entries may lack the `#sha1` suffix Yarn 1 adds to `resolved`
URLs; Yarn accepts them.

A Berry `yarn.lock` stays with Yarn, because aube's Berry output is rejected by
Yarn ([aube#1794](https://github.com/aubepkg/aube/issues/1794),
[aube#1795](https://github.com/aubepkg/aube/issues/1795)). Yarn's version comes
from `packageManager` in `package.json`, then a Yarn 2 or newer pin in
`mise.toml`, then the default below. `bun.lock` stays with Bun.

## Which tool version

Tools run through `mise exec` with an explicit version. A repository's own pins
win: `mise.toml` and `.tool-versions` entries for the relevant tool,
`.python-version`, `.ruby-version`, `global.json`, `packageManager`, and the
highest of the pin and `go.mod`'s `go`/`toolchain` lines for Go. Repository
tasks, environment blocks and hooks are never loaded. Otherwise the runner's
pinned defaults apply:

| Tool | Default |
|---|---|
| Node | 26.13.0 |
| aube | 2.9.0 |
| Yarn | 4.18.1 (Berry) |
| Bun | 1.4.4 |
| Rust | 1.100.0 |
| Go | 1.27.3 |
| Terraform | 1.17.1 |
| Python, uv, Poetry | 3.15.2, 0.13.6, 2.5.1 |
| .NET SDK | 10.0.403 |
| Ruby | 4.0.9 |
| PHP, Composer | 8.5.8, 2.10.3 |
| APM | 0.35.0 |
| gh-actions-lock | 0.4.1 |

### PHP

`lockfiles.php = "static"` (the default) downloads a prebuilt
[static-php](https://static-php.dev) CLI through mise's http backend: no
compiler or system libraries, but it needs an exact `x.y.z` version. `"compiled"`
uses mise's `php` tool, which builds PHP from source on first use and needs a C
toolchain and PHP's build dependencies.

### Nix

nix is not in mise's registry, so the Count installs a pinned static build of
nix 2.35.5 (Hydra's `buildStatic` binary for x86-64 and ARM64 Linux) through
mise's http backend, checked against its SHA-256. It needs no `/nix`, daemon
or root. There is no static nix for macOS, so on a Mac a `flake.lock` update
fails with a clear reason.

nix runs with a private chroot store (`--store local?root=…`) in the tool
store, its own HOME and configuration directory, and only these settings:
`experimental-features = nix-command flakes`, `accept-flake-config = false`
(a flake's `nixConfig` is ignored) and `max-jobs = 0` (nothing is built). The
system and user `nix.conf` are not read. The GitHub token reaches nix only as
`access-tokens = github.com=…`, for its GitHub API calls. Locking reads
`flake.nix` but does not evaluate the flake's outputs, so no repository code
runs.

nix fetches what `flake.nix` declares, so before it runs the Count reads the
declared inputs, nested input overrides included, under nix's restricted
evaluation, which fetches nothing and reads only the flake's directory. An
input `flake.nix` adds or points somewhere refused fails the branch before any
request. After nix runs, every node it added or changed in the lock is checked
the same way, and an update must lock exactly the revision that passed the
lookup and its release-age wait: if the branch moved in between, the update
fails and is retried with the newer revision. Inputs that an upstream flake
declares for itself are fetched by nix before the lock can be checked; a lock
that reaches outside is still never published.

### APM

`apm lock` is APM's lock-only mode. It resolves every package and downloads it
into `apm_modules/` to record its commit and content hashes, but it deploys
nothing to agent directories such as `.github/` or `.claude/`, and runs no
package content: no scripts, hooks or MCP servers. Only `apm.lock.yaml` is
kept; `apm_modules/` never enters the commit. Git may use the `file`
protocol as well as `https` in this run, because APM clones each package from
its own local cache. APM reads only `apm.yml`, so a manifest named
`apm.yaml` gets no lockfile refresh.

### gh-actions-lock

A `.github/workflows/actions.lock` is regenerated only for workflows the
lockfile already lists: `--no-onboard` and `--no-narrow` are always set, so
the Count adds no workflow or action to the lockfile and rewrites no `uses:`
ref he did not edit. An update touching a workflow the lock does not list
leaves it alone, and a run whose edited workflows are all unlisted runs no
tool at all.

An update inside an in-repository `action.yml` or `action.yaml` depends on
how the workflows call it. A local `./` composite is never followed or
locked by the tool, so the update leaves the lockfile alone and publishes
as it would without one. A composite called with the tool's same-repository
syntax, `uses: $/<dir>`, is relocked under the workflows that call it: the
Count finds the listed workflows whose steps reach the file through `$/`,
nested composites included, runs the tool over exactly those, and the
read-back wants the new ref under each of them. If a listed workflow or a
composite on the way cannot be read or parsed, which workflows call it is
unknown and the update is held with the reason. A workflow or action file
that is a symlink is left out of the tool's checkout, so whatever reaches it
fails with the tool's reason, and only the `actions.lock` work is held.

The tool authenticates to github.com only, through `GH_TOKEN`, `GH_HOST` and
`GH_REPO` and nothing else in its environment: on the App server it gets a
scoped installation token; the CLI and the GitHub Action hand it the run's
own token, as Renovate does. `gh` and `git` are kept off its `PATH`, so no
GitHub Enterprise configuration can redirect it. An action the token cannot
read — a private one — holds the update with the reason instead of
publishing a lock that dropped it, and a rate-limit or outage answer makes
the update wait rather than fail.

The tool can report success while still refusing to cover an action, so a
clean exit is not enough: the run then checks the lock itself with the
tool's offline `--verify-local` pass over the same paths, and reads the
regenerated `actions.lock` back — every step-level `uses:` in the files the
run scoped must appear under its workflow (a `$/` composite's under every
workflow that calls it), keyed as the tool keys it (owner
and repository lowercased, no sub-path), and a written SHA must be the
commit the lock records — before the commit opens. Reusable workflows at
`jobs.<id>.uses` are not in the lock and are not checked, and an edited
workflow the lock does not list is not the run's to prove. `actions.lock` is
the only file the tool may change; the usual unexpected-write guard holds
for the rest.

`--no-narrow` also leaves a SHA pin as written: an existing pin the update
moves stays a pin in the workflow, and the lock records it under that SHA
with the tag it resolves to.

[Lockfile maintenance](#lockfile-maintenance) runs the same command with
`--relock` and no workflow paths. The tool resolves every ref the listed
workflows use again, direct and inside composites, and advances each one
that moved upstream to its new commit: a floating ref such as `v7`, or a tag
that was moved forward. A moved ref whose old commit is no longer in the
ref's history is not advanced; it fails the run with the tool's reason, as
does any other blocking finding in any workflow. The tool resolves every
workflow in `.github/workflows/`, listed or not, so a broken ref in a
workflow the lock does not list (a typo, a deleted tag, a private action, a
workflow that does not parse) fails the relock too. A listed workflow that
uses an action the lock does not list yet blocks the relock until the
repository's owner runs `gh actions-lock` to add it. The whole-directory run
also drops listed workflows that no longer exist, never onboards a workflow
or action the lock does not list, and never rewrites a workflow: the
maintenance commit holds `actions.lock` alone. When every listed workflow
is gone, the tool removes `actions.lock` and the maintenance commit deletes
it; the PR says so. The regenerated lock must list exactly the listed
workflows that still exist, or the relock fails, and the proof is
`--verify-local` over those workflows. The maintenance PR lists each
moved ref under its workflow, with the commit it now resolves to and the one
the lock had. No release-age wait applies to what a relock picks up: the
tool takes no cutoff, so a ref that moved an hour ago is locked like any
other.

## The tool store

Tools install into the Count's own mise data directory, separate from yours:
`PATCHULA_TOOLS_DIR`, or `tools/` in the cache directory. It persists
between runs, so keep it on a volume. The Count installs his own pinned mise
there on first use and runs that for every tool and lookup; the mise on `PATH`
only installs and starts it. Each successful tool run records its
`tool@version` and a timestamp.

- `patchula tools install` prewarms every pinned default, for example in an
  image build.
- `patchula tools prune --days 30` uninstalls versions no run has used for 30
  days. Pinned defaults stay. It also empties the private nix store and nix's
  fetcher cache, whatever `--days` says: they hold only downloaded flake
  inputs, and a `nixpkgs` input alone takes several hundred MB. The next nix
  run downloads what it needs again, so run prune on a schedule if you update
  flakes.
- An install that never completed a run is redone from scratch on first use
  (30-minute allowance).
- Known versions keep a 180-second limit per command.

On a local run, Node, Go, Bun, aube, Yarn, uv and Terraform versions you
already have in your own mise (`MISE_DATA_DIR`, or `~/.local/share/mise`, on
the same filesystem as the store) are hard-linked into the store instead of
downloaded. That takes no time or extra disk, and pruning never touches your
installs. Each version is borrowed once; if that fails, mise installs it.
Python, Ruby, PHP and .NET installs depend on where they live, so they are
always installed fresh, as are APM and any install with a symlink leading outside its
own directory. The server never borrows.

## Isolation

Each tool runs in a scratch tree checked out from a persistent partial clone,
containing only the files the tool reads. A repository's branches regenerate
their lockfiles side by side, one per CPU core up to four, each in its own
tree; only the checkout from the shared clone takes turns. Every process starts from an
allowlisted environment with its own HOME, npm config, Cargo home and Go paths.
Parent tokens, cloud credentials, Git helpers and your tool configuration are
not inherited. Git alone gets a temporary credential header for the
installation token, kept out of URLs, arguments and stored config. Yarn runs
the release the Count pins, on a copy of `.yarnrc.yml` without `yarnPath`,
plugins or `${VAR}` interpolations, since the scratch tree holds no `.yarn/`
and the run has none of the variables; a `${VAR}` registry stays, so Yarn
stops on it. npm, pnpm, aube and Bun see each `.npmrc` with only its
`registry=` and `@scope:registry=` lines, so a private scope resolves where
your own installs would, never to the public package of its name.

Install scripts are skipped wherever a tool allows it (npm, aube, which also
writes Yarn Classic locks, Bun, and Composer with plugins off too), because a
lockfile needs none of them.

Output is kept only when something fails, at most 4 KiB of each stream.

:::danger[Lockfile tools run repository and package code]
Regenerating a lockfile runs code from the repository and from its
dependencies: Bundler evaluates the Gemfile and gemspecs, dotnet evaluates
MSBuild projects and targets, uv and Poetry build source distributions to read
their metadata, and Yarn and npm prepare git dependencies by running their
build scripts. The tools run unsandboxed, as the Count's own user, so that code
can read anything the Count can: its environment and settings secrets, the
GitHub App credentials, and what other runs on the same host hold at the time.
Run the Count only for repositories you trust that far, or turn lockfile
updates off for the others.
:::

## Tool caches

Lockfile tools keep the package data they download in `tool-caches/` under
the cache directory, split into partitions so a run never shares a cache
with something it should not reach. A run chooses its partition like this:

- **`public`** — shared by every installation and the CLI. A run may use
  it only when the extraction decided every file it can read is
  public-only: every registry, index or fetch source its manifests,
  settings and lockfiles name is the ecosystem's well-known public
  registry, a well-known public forge, or a local source that fetches
  nothing. Anything uncertain, unreadable or undecided fails closed.
- **`installation-<hash>`** — one partition per installation
  (`installation-<sha256 of the GitHub credentials scope>`, or
  `installation-local` for the CLI), for every run that is not provably
  public-only and holds no operator credentials: a private or unreadable
  `.npmrc`, a Gemfury-style index URL, a lockfile `resolved` URL on a
  non-public host — and equally two installations that happen to name the
  same private registry, which never share a partition.
- **`private-<hash>`** — for runs given operator credentials, named by a
  hash of the credentials' scopes.

There is no read-only lower layer: a run never reads a partition it did
not choose, since a public package of the same name could then satisfy it
without the private feed, and a partition warms after its first run.
Credential files still live only in the run's own directory and are
deleted when it finishes.

Go needs no file verdicts: in `public` it runs with
`GOPROXY=https://proxy.golang.org` — no `direct`, which could fetch
anonymously from a host only the server's network reaches — so what it
can fetch is public by construction. A Go run that fails there, on an
internal module the proxy cannot serve, is rerun in the installation
partition with Go's default environment, so repositories with internal
modules keep working.

Partitions keep honest repositories' private packages apart; they are
not an isolation boundary. Lockfile tools run repository code
unsandboxed — Bundler evaluating the Gemfile, uv or Poetry building
sdists, MSBuild evaluating project files — so code from any installed
repository can read every partition on the host, `public`, another
installation's and another credential set's alike. That is within the
trust model above: every installation is trusted, and lockfile tools run
repository code anyway.

A partition a run uses is held shared, so eviction never removes one in use,
and tools whose caches are not documented for concurrent use — aube, Bun,
Yarn, Composer, Poetry, NuGet and Bundler — run one at a time in it.
Partitions unused for 14 days are pruned, and `lockfiles.cacheGiB` (default
20, minimum 1) caps their size: the least recently used private partitions
go first, `public` last.

## Private registries in lockfile tools

Lockfile tools get a [registry entry's](/guide/sources/#private-registries)
credentials only when the entry sets `tools = true`; APM, Nix, `mise lock` and
the Gradle wrapper read no private registries and get none. In the server,
`allowedInstallations` decides which installations' tools are given them,
and the CLI gives them to every repository it runs on. As lockfile tools run
repository and package code, any repository the Count processes can read a
credential it holds, so neither setting is a boundary.

Each tool gets the credentials in the form it reads, only for its own run:

| Tool | Form | Credentials | Matched by |
| --- | --- | --- | --- |
| aube (npm, pnpm and Yarn Classic locks), npm, Bun | `//host/path/:_authToken` or `:_auth` in its user configuration | bearer, basic | prefix |
| Yarn (Berry locks) | `npmRegistries` in its user configuration | bearer, basic | the registry URL, exactly |
| Cargo | `CARGO_REGISTRIES_<NAME>_INDEX` and `_TOKEN` | bearer, basic | the sparse index URL of each registry `.cargo/config.toml` or `.cargo/config` names |
| dotnet | `NuGetPackageSourceCredentials_<key>` | basic | the URL of each source a `nuget.config` adds |
| Poetry | `POETRY_HTTP_BASIC_<NAME>_USERNAME` and `_PASSWORD`, and `~/.netrc` | basic | the URL of each `[[tool.poetry.source]]`; host |
| Bundler | `BUNDLE_<HOST>` | basic | host |
| Composer | `COMPOSER_AUTH` | bearer, basic | host |
| Go | `~/.netrc`, with the host in `GOPRIVATE` | basic | host |
| uv | `~/.netrc` | basic | host |
| Terraform | `TF_TOKEN_<host>` | bearer | host |

Files are written with owner-only permissions and removed when the tool
finishes. Cargo sends the token as the `Authorization` header, so it gets the
header a lookup sends: `Bearer <token>` or `Basic <base64>`. Cargo runs
outside the repository and reads none of its configuration; it gets each
registry that the `.cargo/config.toml` and `.cargo/config` files at or above
the project define under a `tools` entry, by name, index and token, and
the directory's committed [publish-age
cooldown](/guide/lookup-policy/#cargos-cooldown) — or the job's `waitHours`
when that is longer — by environment, so crates a resolve newly locks wait
as long as the ones an update names. Cargo resolves a lockfile in one
pass, so the job's one wait covers everything that resolve newly locks —
the crate the update names and transitive crates alike. A semver-crossing
update runs `cargo update --workspace`, and when the age check leaves a
release it needs out of reach the failure reads as Cargo's ordinary
refusal, "did not resolve … to requested version", not its publish-age
line. Naming the age explicitly on every
invocation also means a `.cargo/config.toml` above the runner's own
directories cannot impose one the repository never committed. A
`security.apply` fix that lands inside the window is the one job that runs
`resolver.incompatible-publish-age = "allow"`. Rust older than 1.100
ignores the variables: the cooldown still binds the direct targets lookup
picks, but the run warns that transitive crates in that directory do not
wait. dotnet
reads every `nuget.config` in the tree, since a project can name any of them,
and gives no source credentials if it cannot read one; NuGet compares source
keys without case. Poetry reads the project's
`pyproject.toml`. A name defined more than one way, or that the tool would fold onto another
name, gets nothing, and so does a URL not written in its normal form (lowercase host, no
user, escapes or backslashes). Go fetches the modules under a credentialed
host from the host itself, never through the public module proxy or checksum
database.

Bundler, Composer, Go, uv and Terraform send credentials to every URL on the
host, so they only get an entry whose prefix is a bare host, with no path or
port, and that is the only entry on its host, for any installation. A
credential a tool cannot take is left out without a warning, and the tool
then fails on the registry: a bearer entry for Bundler, dotnet, Go, Poetry or
uv, a basic one for Terraform, a `.netrc` user or password with whitespace,
quotes, a backslash or `#`, a NuGet user or password with `;`, a source or
registry name outside ASCII letters, digits, `-`, `_` and (for NuGet and
Poetry) `.`, a Bundler or Terraform host with `_`, a Bundler host without a
dot, a Bundler user or password with characters other than letters, digits
and `-._~!$&'()*,;=` (and `:` in a password),
and a Cargo registry whose index is not `sparse+`. Composer puts basic credentials in the URL of the Git
commands it runs for a VCS repository on that host, where other processes on
the host can see them. No other entry may sit under a `tools` entry's prefix,
because npm, aube and Bun would send the broader entry's credentials there,
and the prefix cannot hold `=` or `;`, which `.npmrc` keys cannot. Lockfile
tools read the registries the repository names, never a
[mirror](/guide/sources/#mirrors).

When a tool fails while it holds credentials, the failure shows none of its
output, which may quote what the private registry answered, only how it
failed: a timeout or its exit status. The repository's
own registry, proxy and TLS settings apply as each tool reads them. npm,
aube and Bun match credentials by host and path whatever the scheme, and
Bundler, Composer, uv and Poetry by host whatever the scheme or port: a
repository that names the host over plain HTTP or on another port, or turns
TLS checks off, or a registry that serves plain-HTTP download URLs, sends the
credential unencrypted or to another service on the host.

## Lockfile maintenance

```pkl
lockfiles {
  refresh = true
  waitHours = 168
}
```

`refresh = true` keeps one `patchula/lock-file-maintenance` branch that
refreshes every lockfile within the manifests' ranges, at most once per ISO
week, and no sooner than `waitHours` after the last successful refresh.
For `.github/workflows/actions.lock` maintenance relocks the refs that
moved upstream in the workflows it lists, as [gh-actions-lock](#gh-actions-lock)
describes, with no release-age wait. When the relock fails, the branch still
carries the other lockfiles' refresh and the failure becomes a report
warning; it fails the branch only when nothing else changed. The
default window is Monday 00:00 to 04:00 UTC; explicit update windows
replace it. Windows work as for updates: the first run after a window time
picks it up, however often the Count runs. A maintenance resolve has no
update to take a wait from, so for Cargo the run holds everything it newly
locks to the strictest active `waitHours` any update class reaches on any
of the repository's crates.io dependencies. When a rule cannot be evaluated
for one of them (a `current` filter that is not a valid requirement, say),
maintenance is held with that error, named for the dependency, rather than
relocking with no wait; a Cargo update's job is held the same way.
Failures do not count as a completed refresh, and a closed maintenance PR can
come back the following week.

`tidyGo = true` runs `go mod tidy` instead of `go mod download` after Go updates.
