---
title: "Release age and channels"
description: "How the Count picks the version to propose: update classes, the release-age wait, prereleases, ranges and Node release channels."
---
For each dependency and each update class (major, minor, patch), the Count
proposes the newest version that is permitted, not deprecated, not yanked and
old enough. Prereleases are only considered when the current version is
already a prerelease, or when a rule's `permitted` filter asks for them.

A release is a prerelease when its version says so (`2.0.0-rc.1`, `3.0rc1`,
`1.5-beta1`), or when its source marks it one: a GitHub release flagged as a
prerelease, listed through mise. A `permitted` filter asks for a prerelease
only on purpose; one that just narrows or excludes versions does not:

- A `regex:` filter asks when it matches the prerelease but not the same
  version without its prerelease part, so the prerelease part is what
  matched: `regex:-rc\.` and `regex:nightly` ask, `regex:^2\.` does not.
  A version that is a prerelease only by its source's flag has no
  prerelease part, so no `regex:` filter asks for it.
- A `semver:` range asks when it names a prerelease version itself and
  the prerelease is inside it: `semver:>=3.0.0-rc.1` or `semver:[1.0-beta1,2)` ask,
  `semver:^2` and `semver:[1,2)` do not.
- A negated filter (`!regex:...`, `!semver:...`) only excludes and never
  asks.

The current version comes from the lockfile when there is one. An npm range on
its own only tells the Count which class an update belongs to; he never
pretends the range's floor is what you have installed.

## The release-age wait

`waitHours` defaults to 24, the same as mise's `minimum_release_age`. A release
with a publication date becomes eligible that many hours after it was
published. Brand-new releases are where broken and malicious versions tend to
show up, and a day is usually enough for someone else to notice.

- Releases the registry gives no date have no wait. `waitHours = 0` turns
  the wait off.
- A release whose date the Count could not read, because the request for it
  failed or the registry has not described it yet, waits until a later
  lookup reads one; the report lists it as a notice. Go, Maven and NuGet
  date every version, not just the newest, so a new patch release on an
  older line waits like any other.
- [Reference data](/guide/configuration/#reference-data), such as CA root
  bundles, has no wait by default.
- Versions you already use, and versions pinned in lockfiles, are never held
  back by age.
- The report lists held-back releases with `publishedAt`, `eligibleAt` and
  `updateType`, even when nothing is eligible yet. The [dashboard](/guide/dashboard/)
  shows them too, with a box to create one early.
- One instant per batch decides eligibility, so every repository in a run sees
  the same answer.

### Cargo's cooldown

A repository can commit a publish-age cooldown that its own Cargo enforces:
`registry.global-min-publish-age`, `registry.min-publish-age` or
`registries.<name>.min-publish-age` in `.cargo/config.toml` or `.cargo/config`,
at the root, in a member or in a directory above one inside the repository.
The Count reads the strictest of every applying value in Cargo's own duration
grammar (`N seconds/minutes/hours/days/weeks/months`, a month being
2,629,746 seconds, converted to hours rounding up) and applies it as a floor
under `waitHours` for the crates.io dependencies the configuration reaches: a
release inside the window is never proposed, however short the configured
wait, and a dashboard "create it now" tick cannot beat it either.

- A committed `resolver.incompatible-publish-age = "allow"` is ignored: the
  Count stays stricter, never looser.
- A cooldown configuration the Count cannot read — `include`, unparseable
  TOML or a value outside the grammar — fails that manifest's crates.io
  lookups rather than run unprotected, and fails that directory's lockfile
  jobs rather than run Cargo with no cooldown.
- Security fixes bypass the floor: a release that fixes an advisory is
  proposed inside the window anyway, and the report marks it so.
- The lockfile run's Cargo is told: every invocation gets the committed
  value (or the job's `waitHours`, whichever is longer) by environment, so
  direct targets, transitive crates and maintenance all wait the same way,
  and only the job whose security fix sits inside the window runs
  `resolver.incompatible-publish-age = "allow"`. A Rust toolchain older
  than 1.100 ignores the variables — the floor still binds the direct
  targets lookup picks, but the run warns that transitive crates there do
  not wait.

GitHub tags take their date from the matching release's publication time, then
from an annotated tag's tagger date. A lightweight tag with no release is
undated. Commit timestamps are never used: a commit made months ago and tagged
today is a release from today.

```pkl
rules {
  new Rule {
    note = "Our own packages can skip the wait."
    select { packages { "glob:@acme/*" } }
    apply { waitHours = 0 }
  }
}
```

## Ranges, peers and providers

How a declaration is rewritten, peer ranges, Cargo floors and Terraform
constraints included, is under [version policies](/guide/versioning/#version-policies).

## Release channels

Newest stable is the default. `channel = "lts"` follows the line the source's
catalog marks with an `lts` dist-tag. For mise's `node`, the Count reads the
LTS line from `mise ls-remote node lts`; for setup-node's
`actions/node-versions`, from Node's distribution index
(`nodejs.org/dist/index.json`). Any other source whose catalog carries an
`lts` dist-tag works the same way:

```pkl
rules {
  new Rule {
    note = "Use Node LTS"
    select {
      managers { "mise"; "github-actions" }
      packages { "node"; "core:node"; "actions/node-versions" }
      kinds { "tool" }
    }
    apply { channel = "lts" }
  }
}
```

Candidates stay on the `lts` tag's major line. The release-age wait and
`permitted` filters still apply. If the catalog has no `lts` dist-tag, or its
value does not parse as a version, the Count reports it rather than quietly
falling back to stable.

To stay on a fixed line for something that is not Node itself, such as
`@types/node`, say so with a filter:

```pkl
rules {
  new Rule {
    select { packages { "@types/node" } }
    apply { permitted = "semver:^24" }
  }
}
```
