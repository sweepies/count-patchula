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
| `pnpm-lock.yaml` | aube | `aube install --lockfile-only --ignore-scripts` |
| `package-lock.json`, `npm-shrinkwrap.json` | aube, or npm | `npm install --package-lock-only --ignore-scripts` |
| `yarn.lock` | Yarn Classic or Berry | `yarn install --ignore-scripts`, or `--mode=update-lockfile` |
| `bun.lock` | Bun | `bun install --lockfile-only --ignore-scripts` |
| `Cargo.lock` | Cargo | `cargo update -p <crate>@<old> --precise <target>` |
| `go.sum` | Go | `go get <module>@<target>`, then `go mod download` or `go mod tidy` |
| `.terraform.lock.hcl` | Terraform | `terraform providers lock` |
| `uv.lock` | uv | `uv lock --upgrade-package <package>` |
| `poetry.lock` | Poetry | `poetry lock --no-plugins` |
| `packages.lock.json` | dotnet | `dotnet restore --force-evaluate` |
| `Gemfile.lock` | Bundler | `bundle lock --update <gems>` |
| `composer.lock` | Composer | `composer update <packages> --with-all-dependencies --minimal-changes --no-install --no-scripts --no-plugins` |
| `mise.lock` | mise | `mise lock` with the lockfile's recorded platforms |
| `flake.lock` | nix | `nix flake update <input>`, or `nix flake update` for maintenance |

Lifecycle scripts never run. A lockfile that does not change after an edit that
changed recorded dependencies fails the branch, so a tool that says "up to date"
without re-resolving cannot publish a stale lock. An edit that cannot affect the
lock, such as a description, is not a failure. Where the Count can read the
lock back (npm, Bun, Cargo, uv, Poetry, Bundler, Composer, Terraform
providers), the requested version must be the one the new lock records.

:::caution[Not supported]
Binary `bun.lockb` is not supported, and neither is regenerating `Chart.lock`
or `gradle.lockfile`. Updates to those hold as lockfile-pending.
:::

## Why aube for npm and pnpm

The package manager sets a floor on how fast an update can be. Manifest edits
and API commits take milliseconds; the lockfile tool takes seconds.

[aube](https://aube.sh) reads and writes pnpm and npm lockfiles in place. On a
project with 265 locked packages, refreshing the lock after a manifest edit took
0.11 s with aube on the pnpm lock and 0.12 s on the npm lock, against 2.48 s
with pnpm 10 and 1.46 s with npm 11, all with warm metadata caches. So the Count
uses aube for both.

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

## The tool store

Tools install into the Count's own mise data directory, separate from yours:
`PATCHULA_TOOLS_DIR`, or `<cache>.tools` beside the SQLite cache. It persists
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
- A version the store has never run successfully is reinstalled from scratch on
  first use, with a 30-minute install allowance. This recovers from an install
  cut short by a timeout or crash, which can leave a directory mise treats as
  installed but with no binaries.
- Known versions keep a 180-second limit per command.

On a local run, Node, Go, Bun, aube, Yarn, uv and Terraform versions you
already have in your own mise (`MISE_DATA_DIR`, or `~/.local/share/mise`, on
the same filesystem as the store) are hard-linked into the store instead of
downloaded. That takes no time or extra disk, and pruning never touches your
installs. Each version is borrowed once; if that fails, mise installs it.
Python, Ruby, PHP and .NET installs depend on where they live, so they are
always installed fresh, as is any install with a symlink leading outside its
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
