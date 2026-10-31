---
title: "Lockfiles"
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
| `yarn.lock` | Yarn Classic or Berry | `yarn install --ignore-scripts`, or `--mode=update-lockfile` |
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

A `mise.lock` records the configuration in its own directory and above it,
as mise does. A `mise.toml` in a subdirectory is locked by a `mise.lock` of its
own, and with none beside it the Count updates the manifest alone. Tools with no
platform entries, such as `rust`, lock for the platforms the rest of the
lockfile records.

A Cargo update across a semver-incompatible boundary, such as `0.22` to `0.23`
or `1.x` to `2.x`, adds a second entry beside the old one, because other crates
may still require it. The Count runs `cargo update --workspace` for it, which
locks what the edited manifests need and moves no other package, then pins the
entry to the target with `--precise` if Cargo locked a newer release.

Lifecycle scripts never run. A lockfile that does not change after an edit that
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
  manifest's own dependency (for Cargo, the dependency edge of the manifest's
  package, not another version of the same crate locked for a dependent), and the
  new lock must record the target or nothing is published.
- Without a committed lockfile there is nothing to change, so the release is
  not an update. npm and Bun have no such command, so they only get this when
  the scheduled refresh below runs.

## Why aube for npm and pnpm

[aube](https://aube.sh) rewrites pnpm and npm lockfiles in place about 10 to 20
times faster than pnpm or npm (0.1 s against 1.5 to 2.5 s on a 265-package
project), so the Count uses it for both.

aube's npm lockfiles stay valid for `npm ci`,
but a later `npm install` may rewrite a few lines (hoisting and `funding`
metadata). On larger pnpm graphs aube may keep a second copy of a package pnpm
would deduplicate. `lockfiles.npmEngine = "npm"` restores native npm for
`package-lock.json`. Native npm is also used automatically for format v1 locks,
which aube cannot read. Trees with local `file:` dependencies stay with aube,
which keeps them valid since 2.8.1
([aube#1796](https://github.com/aubepkg/aube/issues/1796)).

`yarn.lock` stays with Yarn, because aube's Yarn Berry output is rejected by Yarn
([aube#1794](https://github.com/aubepkg/aube/issues/1794),
[aube#1795](https://github.com/aubepkg/aube/issues/1795)). Yarn's version comes
from `packageManager` in `package.json`, then a `yarn` pin in `mise.toml`, then the
committed lock's format. `bun.lock` stays with Bun.

## Which tool version

Tools run through `mise exec` with an explicit version. A repository's own pins
win: `mise.toml` and `.tool-versions` entries for the relevant tool,
`.python-version`, `.ruby-version`, `global.json`, `packageManager`, and the
highest of the pin and `go.mod`'s `go`/`toolchain` lines for Go. Repository
tasks, environment blocks and hooks are never loaded. Otherwise the runner's
pinned defaults apply:

| Tool | Default |
|---|---|
| Node | 24.21.0 |
| aube | 2.8.1 |
| Yarn | 1.22.22 (Classic), 4.18.1 (Berry) |
| Bun | 1.4.3 |
| Rust | 1.99.0 |
| Go | 1.27.2 |
| Terraform | 1.14.7 |
| Python, uv, Poetry | 3.14.8, 0.13.1, 2.5.1 |
| .NET SDK | 9.0.319 |
| Ruby | 3.4.11 |
| PHP, Composer | 8.5.8, 2.10.3 |
| APM | 0.35.0 |

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
containing only the files the tool reads. Every process starts from an
allowlisted environment with its own HOME, npm config, Cargo home and Go paths.
Parent tokens, cloud credentials, Git helpers and your tool configuration are
not inherited. Git alone gets a temporary credential header for the
installation token, kept out of URLs, arguments and stored config. Yarn runs on
a copy of `.yarnrc.yml` without `yarnPath`, plugins or `${VAR}` interpolations,
since a committed Yarn release and plugins are repository code.

Output is kept only when something fails, at most 4 KiB of each stream.

:::note[Private registries]
Private registries do not work yet, because credentials are scrubbed on
purpose. Scoped registry credentials will be a separate, explicit opt-in.
:::

## Lockfile maintenance

```pkl
lockfiles {
  refresh = true
  waitHours = 168
}
```

`refresh = true` keeps one `patchula/lock-file-maintenance` branch that
refreshes every lockfile within the manifests' ranges, at most once per ISO
week, and no sooner than `waitHours` after the last successful refresh. The
default window is Monday 00:00 to 04:00 UTC; explicit update windows
replace it. Windows work as for updates: the first run after a window time
picks it up, however often the Count runs.
Failures do not count as a completed refresh, and a closed maintenance PR can
come back the following week.

`tidyGo = true` runs `go mod tidy` instead of `go mod download` after Go updates.
