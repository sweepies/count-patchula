---
title: "Versioning"
---
Each dependency has a version scheme. It decides how versions sort, which update
class a change belongs to, whether a version satisfies a range, and how a range
is rewritten.

| Scheme | Used by | Notes |
|---|---|---|
| `semver` | npm, Cargo, most GitHub sources | npm and Cargo requirements are parsed separately: bare `1.2.3` is exact in npm and compatible in Cargo |
| `docker` | container images | a numeric prefix plus a variant suffix that never changes (`-alpine`, `-bookworm`) |
| `pep440` | requirements, PEP 621, uv | epochs, pre/dev/post and local versions, `~=`, wildcards and exclusions |
| `poetry` | Poetry | `^` keeps the leftmost nonzero part, `~` allows patches (minors for a bare major), `\|\|` separates alternatives |
| `go` | Go modules | canonical `v` SemVer and pseudo-versions |
| `hashicorp` | Terraform | `=`, `!=`, comparisons, `~>` (two- and three-part forms differ) |
| `maven` | Maven, Gradle | Maven's ordering: `alpha < beta < milestone < rc < snapshot < release < sp`; release variants such as `-jre` are kept |
| `nuget` | NuGet | SemVer 2 with an optional fourth number; interval ranges like `[2.9.0,3.0)` |
| `rubygems` | Bundler | letters make a prerelease (`1.0.0.rc1 < 1.0.0`), `~>` |
| `composer` | Composer | up to four numbers, `alpha < beta < RC < stable < patch`, `^`, `~`, wildcards, hyphen ranges, `\|\|` |
| `loose` | mise tools, toolchains | numeric dotted versions with an optional `v` |

## Update classes

Major, minor and patch follow each scheme's own components. Cargo counts a
change in the leftmost nonzero component as major, so `0.3` to `0.4` is major,
as Cargo's compatibility rules say. A Python epoch change is major. Prereleases
are only proposed as [release age and channels](/guide/lookup-policy/)
describes.

## Version policies

`versions` decides what happens to the declaration:

- `exact` pins the target. Cargo writes `=1.2.3`, because a bare version in
  Cargo allows compatible releases. Python writes `==`.
- `preserve` leaves a range alone while it is still satisfied, and shifts
  simple caret, tilde and wildcard ranges when it is not (`^6.4` becomes `^7.1`).
- `widen` extends the range to include the new version: a union for npm
  (`^5 || ^7.0.0`) and a convex interval for Cargo, which cannot express
  unions.

A compound range that cannot be rewritten without guessing (`>=1.0 <2.0`
needing `3.0`) fails with a warning instead of inventing something.

### The default depends on the ecosystem

The default is `exact` for most dependencies, and `preserve` for Cargo,
pyproject (PEP 621 and Poetry), Bundler and Composer. A built-in rule in the
schema sets it, with a note saying why: these ecosystems resolve one version
per package, and a lockfile records it.

- A pin adds nothing over the lockfile. The lock already says exactly which
  version is installed.
- A pin can clash with other requirements on the same package. A direct
  `=0.23.1` in Cargo fails to resolve when another crate in the tree needs
  `^0.23.2`, where a range lets the resolver pick one version for both.

`requirements.txt` stays `exact`: `==` is the norm there, and the file acts as
the lock. npm and Bun stay `exact` too, so installs are reproducible without
relying on a lockfile alone; see [Exact npm versions](/renovate/departures/#npm-range-edits).

Under `preserve` a release the declaration already allows does not edit the
manifest. When the manifest has a committed lockfile, the update moves that
lock to the new version and nothing else, so `tokio = "1"` locked at 1.39.0
still gets 1.40.0:

| Lockfile | How the lock moves to the chosen version |
|---|---|
| `Cargo.lock` | `cargo update -p tokio@1.39.0 --precise 1.40.0` |
| `uv.lock` | `uv lock --upgrade-package tokio==1.40.0` |
| `poetry.lock` | Poetry cannot be told a version, so the constraint is pinned for one `poetry lock` and restored for a second |
| `Gemfile.lock` | Bundler cannot be told a version either, so the requirement is pinned for one `bundle lock --update` and restored for a second `bundle lock` |
| `composer.lock` | `composer update <package> --with <package>:1.40.0` |

The PR, branch and commit read like any other update (`update tokio to 1.40.0`),
and the [release-age wait](/guide/lookup-policy/#the-release-age-wait),
windows, grouping and `permitted` all apply. The Count reads the new lock back
and refuses to publish if it does not record the target. The version the
update starts from is the locked one, not the range's floor.

With no committed lockfile there is nothing to change, so a release inside the
range is not an update. npm and Bun lockfiles have no such command: if you set
`versions = "preserve"` for them, an in-range release is still left out, and
only the [scheduled lockfile refresh](/guide/lockfiles/#lockfile-maintenance)
moves the lock.

To go back to pins everywhere, add a rule after the built-in one:

```pkl
rules {
  new Rule {
    note = "Pin everything."
    select { managers { "cargo"; "pyproject"; "bundler"; "composer" } }
    apply { versions = "exact" }
  }
}
```

The built-in rules come first, so a rule of yours overrides them. Assigning a
new Listing to `rules` drops them, and with them this default and the
[reference data](/guide/configuration/#reference-data) fast track.

Installed npm dependencies follow `versions`, so by default they are pinned
exactly. Peer dependencies describe what consumers may use, so they are
treated differently: a range widens at a new major (`^5` becomes
`^5 || ^7.0.0`), an exact peer bumps in place, and exact policy never pins a
peer range.

Cargo's `preserve` keeps a satisfying floor, and a two-part requirement that
crosses a major starts at the new major's floor (`0.30` becomes `1.0` for
1.1.0). A Terraform provider constraint that already allows the new version is
left as written, even under exact policy.

## Docker tags

Numeric tags can have more than three parts, and the suffix after the number is
a compatibility label, not a prerelease. `3.12-alpine` upgrades to
`3.13-alpine`, never to `3.13-slim`. Floating tags such as `latest` and opaque
ones get digest updates only.
