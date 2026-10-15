# Deliberate departures from Renovate

rustovate is not a Renovate clone. Each entry records a behaviour that differs
on purpose, and why. The comparison harness links differences it finds to
entries here; a difference with no entry is treated as a bug.

Stable comparison link: <a id="departure-7"></a>**Departure 7: default policy**
(table row 7 below). IDs are retained when descriptions change.

| # | Area | Renovate | rustovate | Why |
|---|---|---|---|---|
| 1 | Config | JSON/JSON5 with presets, migrations | Pkl only, typed schema, unknown fields are errors | Typed, composable config with real diagnostics |
| 2 | Onboarding | Opens an onboarding PR in repos without config | Repos without config are skipped | Configs are added deliberately |
| 3 | Execution | One repository at a time, caches reset per repository | One process per batch, concurrent repos, batch-wide lookups | Speed |
| 4 | Release history | Fetches full release and tag lists | Incremental, newest-first, stops at the first known item | Speed and rate limits |
| 5 | Repository access | Clones each repository | Reads files through the API; clones only for lockfile tools | Speed |
| 6 | Caller interface | Log lines | Versioned JSON report | Logs are not an API |
| 7 | Default policy | Repository configs may select opaque preset names | Explicit schema defaults: exact versions, action/image digests, manual merges, separate PRs, release notes, dependency label and semantic prefix | Auditable independent policy; no preset expansion or AGPL data |
| 8 | Limits | Zero can represent unlimited limits | Null means unlimited by default; positive integers limit PRs; zero is rejected | Avoid overloading a numeric limit |
| 9 | Match syntax | Our configs mix slash-delimited regexes and globs | Explicit `regex:` or `glob:` prefixes, optional `!`; path-aware globs by default | Avoid delimiter guessing; preserve literal slashes |
| 10 | Rule vocabulary | Our configs use packageRules and numerous match/set keys | Typed selectors and policy overrides; ordered replacement, including whole lists; empty group means separate PRs | Smaller, explicit policy model without compatibility aliases |
| 11 | Custom extraction | Our configs use regex/custom managers and currentValue/depName captures with template strings | One extractor model; current/source/dependency captures and simple `{capture}` substitution; Rust regex syntax | One validated model without executable template helpers or lookaround |
| 12 | Update taxonomy | Distinguishes pinDigest and lockFileMaintenance | Both version/digest additions are pin; lockfile refresh is a separate policy | Keep version classification separate from maintenance |
| 13 | Shared configuration | Our configs use named presets and local> references | Ordinary local Pkl amends/import; vendor the bundled schema; no remote imports or external resource reads in Phase 0 | Deterministic validation with no network or environment dependency |
| 14 | Release timing | Duration strings, schedules and timezone keys | Nonnegative waitHours plus explicit cron/timezone windows; empty windows means unrestricted | Typed units and validated timezones; no natural-language scheduling |
| 15 | Repository scanning | Our configs use ecosystem names such as gomod, pip_requirements, custom.regex and managerFilePatterns | Closed manager identifiers (go, requirements, regex, compose); explicit per-manager file sets; no legacy aliases | Fail on typos and keep the independent vocabulary small |
| 16 | Branches and commit messages | Existing configs choose Renovate names or semanticCommits switches | Default rustovate/ branch prefix and explicit chore(deps): commit prefix, both overridable | Independent identity and predictable messages |
| 17 | Maintenance and ancillary policy | Config migration, abandonment and post-update options | No migration or inferred abandonment policy; lockfile refresh and Go tidy are explicit fields | Pkl-only configuration and deliberate, reviewable maintenance |
| 18 | Group identifiers | Separate groupName and groupSlug fields | One group string identifies the update group; branch slug derivation belongs to the updater | Avoid two configuration identities for the same grouping intent |
| 19 | Configuration dependencies | The renovate-config manager treats Renovate configuration as package files | No equivalent manager: the comparison adapter excludes only renovate-config snapshots and summaries | rustovate uses Pkl and does not resolve Renovate configuration or presets |
| 20 | Cargo compatibility | Numeric update levels may differ in black-box reports | A changed pre-1.0 Cargo compatibility boundary is major; exact Cargo uses = | Follow Cargo's documented compatibility rules |
| 21 | Source refresh | Histories are exposed through registry/platform responses | Conditional head probes gate GraphQL; registry bodies use declared freshness | Avoid unnecessary histories and respect actual upstream HTTP capabilities |
| 22 | Repeated dependencies | Manager snapshots can contain repeated dependency names | Optional occurrence IDs preserve different current values, merging identical rows | Retain all manifest occurrences without ambiguous report identities |
| 23 | Minimum release age | The compared run waits about three days | Follow mise's default minimum_release_age of 24 hours for dated releases; choose the newest eligible release and report held-back versions | A short explicit default with release-date evidence; undated releases have no delay |
| 24 | npm integrity | Lookup reports npm integrity as newDigest | npm manifests do not pin integrity; compare targets without npm newDigest | Lockfile integrity belongs to lockfile tooling |
| 25 | npm major range edits | The compared policy retains a range such as ^7.0.0 | versions = "exact" proposes the exact selected release, such as 7.0.2 | Apply explicit version policy consistently on major updates |

<a id="departure-23"></a>**Departure 23: minimum release age.**
The default follows [mise's minimum_release_age](https://mise.jdx.dev/configuration/settings.html#minimum_release_age):
24 hours when a release timestamp is available. The comparison rule requires an
explicit 24-hour policy and timestamp evidence that rustovate's proposal is at
least 24 and less than 72 hours old, plus a strictly older Renovate target in
the same update class. Reversed, undated, older-than-window, unrelated range,
missing, and extra proposals are never excused. This is a narrow explanation of
the observed three-day comparison window, not a claim about all Renovate defaults.

<a id="departure-24"></a>**Departure 24: npm integrity normalization.**
The adapter ignores only proposed newDigest for dependencies whose datasource is
npm (falling back to npm/bun manager when source metadata is absent), including
custom regex extraction of npm versions. Current digests, other sources, version
targets and update types remain comparable. Triage counts normalized proposals.

<a id="departure-25"></a>**Departure 25: exact npm major proposals.**
The narrow rule requires identical resolved targets on both sides, a major
upgrade from a caret/tilde current range, a rustovate exact target with policy
metadata, and a Renovate caret/tilde range with the target's major/minor and a
no-greater patch. Other target, update-type, or range differences stay actionable.

<a id="departure-19"></a>**Departure 19: configuration dependency scope.**
The fixed comparison rule `renovate-config-manager` excludes only Renovate's own
`renovate-config` manager before normalization. Triage records the repository,
manager, file/dependency/update counts and this link under `exclusions`, separately
from actionable differences. No config-file contents or preset data are read or
interpreted. All other managers, including unknown names, remain in comparison.
This adapter scope rule also applies to branch upgrade summaries and is independent
of the optional update classification rules in `conformance/departures.json`.

<a id="departure-20"></a>**Departure 20: ecosystem compatibility and range edits.**
Cargo's pre-1.0 breaking boundaries use its documented leftmost-nonzero rule;
npm classifications retain numeric SemVer components. Exact Cargo versions use
`=`. npm widen retains the original range as a union with the target; Cargo
uses a convex interval because Cargo requirements cannot express unions.
Compound preserve edits with no safe operator-preserving representation report
a warning instead of inventing a range. The narrow cargo-pre1-breaking rule
classifies only minor/patch-to-major taxonomy differences at those boundaries,
with identical update targets. It never excludes version or range differences.

<a id="departure-21"></a>**Departure 21: explicit freshness and GitHub history gates.**
GraphQL POSTs have no conditional HTTP validators, so a conditional REST head
probe gates incremental GitHub history. Registry responses honor declared
max-age (unless no-cache), including npm/sparse responses with validators. The
Go proxy's list/latest responses provide max-age without validators. Fresh bodies
are reused; expired bodies are conditionally validated where supported.
Unchanged warm network responses are 304; fresh registries need no request.
Historical changes behind an
unchanged GitHub head require future invalidation/reconciliation. This affects
request strategy, not a confirmed proposal difference, so it has no comparison
exemption.

<a id="departure-22"></a>**Departure 22: repeated dependency identities.**
The v1 report now has optional occurrence IDs for repeated manager/name pairs
with different package/current fields, including multiple action refs and mise
versions. Identical occurrences merge their proposals and locations. The
black-box adapter applies the same normalization, so no comparison exclusion is
needed. Unlabelled duplicate identities remain invalid.
