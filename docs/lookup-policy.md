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
