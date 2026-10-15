# Lookup policy (SWE-101)

Policy starts at explicit defaults and applies matching rules in declaration
order, independently for each update type. Populated selector fields are AND;
patterns within one field are OR, with exclusions winning. Lists replace lists.
An empty group removes grouping. ignore and exclude suppress proposals.

The newest permitted, nondeprecated, nonyanked version is proposed per numeric
update class. Prereleases require a prerelease baseline or an explicit permitted
filter, allowing the remote-agent nightly rule. Resolved lock versions provide
the baseline where available; npm ranges use their mathematical lower boundary
only for classification, never as a fabricated resolved report version.

`waitHours` defaults to 24, following mise's minimum_release_age. Dated releases
become eligible at publication plus that many hours, inclusive. Lookup chooses
the newest eligible alternative per update class. Dependency metadata reports
`heldBack` releases with `publishedAt`, `eligibleAt` and `updateType`, even if no
proposal is eligible. Undated releases have no wait; `waitHours = 0` disables it.
Extracted current versions and existing lockfile pins remain valid regardless of
age. Rules can replace waitHours per dependency/update class. One UTC instant
per batch determines eligibility; invalid supplied timestamps diagnose an error.

For GitHub tags, the matching release's `publishedAt` takes priority. Without a
release, an annotated tag uses its tagger date. Lightweight tags without a
release are undated and have no wait. Commit timestamps never determine release
age. Release history revalidates independently of the tag namespace, so publishing
a release after its tag updates the date even with a warm cache.

Installed npm dependencies and devDependencies follow `versions`, including
exact pinning. Peers describe consumer compatibility: a range widens at a new
major (`^5` becomes `^5 || ^7.0.0`), and an exact peer bumps in place
(`2026.9.8` becomes `2026.10.1`). Exact policy never pins a peer range. Cargo
preserve keeps a satisfying original floor; a bare two-component upgrade across
numeric majors starts at the new major's floor (`0.30` to `1.0` for 1.1.0).
Satisfying Terraform provider constraints retain their original declaration
even under exact policy. New versions outside that constraint use the configured
version policy.

## Node release channels

Newest eligible stable is the default. To opt into the newest published LTS
line for mise Node and setup-node inputs, add this explicit rule:

```pkl
rules = new Listing {
  new Rule {
    note = "Use Node LTS"
    select {
      managers = new Listing { "mise"; "github-actions" }
      packages = new Listing { "node"; "core:node"; "actions/node-versions" }
      kinds = new Listing { "tool" }
    }
    apply { channel = "lts" }
  }
}
```

`channel` is typed (`stable` or `lts`) and follows ordered rule replacement.
The LTS major comes from the highest version marked with an LTS codename in
Node's [distribution index](https://nodejs.org/dist/index.json), not an even-major
heuristic. The setup-node catalog retains its own targets and release dates and
uses that same Node metadata for the LTS line. Normal permitted filters and
release-age gates still apply. Missing LTS evidence or LTS on another source
reports a diagnostic instead of silently selecting stable. Existing current
versions remain inventory, even if they are outside the selected channel.

For a deliberate fixed support line (including npm `@types/node`), a separate
rule can use `apply { permitted = "semver:^24" }`. Choose that line explicitly;
the npm package does not inherit the Node binary's channel. No example repository
is silently changed to LTS.

Actions retain opaque branch refs. Their digest is resolved for that same ref;
the ops-workflows rule disables digest changes. Version tags can upgrade and be
pinned to their commit. SHA pins without a version comment cannot safely select
a replacement tag and remain unchanged with a warning from the engine.
With a version comment, current.value/current.version are the comment's tag and
current.digest is the SHA; proposals carry a new tag and digest. The pure action
replacement helper preserves subpaths, quoting and the rest of the comment.
Hosted runner `latest` values float without proposals; versioned labels use the
built-in catalog and keep their OS/architecture family.

The v1 update has optional `policy` metadata containing waitHours, windows,
merge, labels, commitPrefix, releaseNotes, bodyNotes and versions. Release-age
gates execute during selection; cron windows are reported for the future updater.
Dated proposals carry `releaseAge` evidence (publishedAt/evaluatedAt).
Existing v1 payloads deserialize without this
metadata; the conformance comparison continues comparing its original fields.

Tests use fixed catalogs with the renovate-runner, fnox and remote-agent example
configs, plus ordered filters, exclusions, empty-list overrides and windows.
