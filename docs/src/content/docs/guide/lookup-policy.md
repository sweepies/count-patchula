---
title: "Release age and channels"
description: "How the Count picks the version to propose: update classes, the release-age wait, prereleases, ranges and Node release channels."
---
For each dependency and each update class (major, minor, patch), the Count
proposes the newest version that is permitted, not deprecated, not yanked and
old enough. Prereleases are only considered when the current version is
already a prerelease, or when a rule's `permitted` filter asks for them.

The current version comes from the lockfile when there is one. An npm range on
its own only tells the Count which class an update belongs to; he never
pretends the range's floor is what you have installed.

## The release-age wait

`waitHours` defaults to 24, the same as mise's `minimum_release_age`. A release
with a publication date becomes eligible that many hours after it was
published. Brand-new releases are where broken and malicious versions tend to
show up, and a day is usually enough for someone else to notice.

- Undated releases have no wait. `waitHours = 0` turns the wait off.
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
  lookups rather than run unprotected.
- Security fixes bypass the floor: a release that fixes an advisory is
  proposed inside the window anyway, and the report marks it so.
- Cargo itself is not told yet: today the floor decides what is proposed and
  what is held; the lockfile run's Cargo gets the cooldown from a later
  change.

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
catalog marks with an `lts` dist-tag. For mise's `node` and setup-node's
`actions/node-versions`, the Count reads the LTS line from Node's distribution
index (`nodejs.org/dist/index.json`). Any other source whose catalog carries an
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
