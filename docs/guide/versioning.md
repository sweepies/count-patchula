# Versioning

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
need opting in.

## Version policies

`versions` decides what happens to the declaration:

- `exact`, the default, pins the target. Cargo writes `=1.2.3`, because a bare
  version in Cargo allows compatible releases. Python writes `==`.
- `preserve` leaves a range alone while it is still satisfied, and shifts
  simple caret, tilde and wildcard ranges when it is not (`^6.4` becomes `^7.1`).
- `widen` extends the range to include the new version: a union for npm
  (`^5 || ^7.0.0`) and a convex interval for Cargo, which cannot express
  unions.

A compound range that cannot be rewritten without guessing (`>=1.0 <2.0`
needing `3.0`) fails with a warning instead of inventing something.

## Docker tags

Numeric tags can have more than three parts, and the suffix after the number is
a compatibility label, not a prerelease. `3.12-alpine` upgrades to
`3.13-alpine`, never to `3.13-slim`. Floating tags such as `latest` and opaque
ones get digest updates only.
