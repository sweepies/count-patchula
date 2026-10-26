---
title: "Deliberate departures"
---
The Count is not a Renovate clone. Where he behaves differently on purpose, the
difference is written down here with a reason.

## Configuration

### Pkl configuration {#pkl-configuration}

- **Renovate:** JSON/JSON5 with presets, migrations
- **Count Patchula:** Pkl only, typed schema, unknown fields are errors
- **Why:** Typed, composable config with real diagnostics

### Explicit defaults {#explicit-defaults}

- **Renovate:** Repository configs may select opaque preset names
- **Count Patchula:** Explicit schema defaults: exact versions, action/image digests, manual merges, separate PRs, release notes, dependency label and semantic prefix
- **Why:** Auditable independent policy; no preset expansion or AGPL data

### Limits {#limits}

- **Renovate:** Zero can represent unlimited limits
- **Count Patchula:** Null means unlimited by default; positive integers limit PRs; zero is rejected
- **Why:** Avoid overloading a numeric limit

### Prefixed patterns {#match-syntax}

- **Renovate:** Our configs mix slash-delimited regexes and globs
- **Count Patchula:** Explicit `regex:` or `glob:` prefixes, optional `!`; path-aware globs by default
- **Why:** Avoid delimiter guessing; preserve literal slashes

### Rules and selectors {#rules}

- **Renovate:** Our configs use packageRules and numerous match/set keys
- **Count Patchula:** Typed selectors and policy overrides; ordered replacement, including whole lists; empty group means separate PRs
- **Why:** Smaller, explicit policy model without compatibility aliases

### One extractor model {#custom-extractors}

- **Renovate:** Our configs use regex/custom managers and currentValue/depName captures with template strings
- **Count Patchula:** One extractor model; current/source/dependency captures and simple `{capture}` substitution; Rust regex syntax
- **Why:** One validated model without executable template helpers or lookaround

### Sharing configuration {#shared-configuration}

- **Renovate:** Our configs use named presets and local> references
- **Count Patchula:** Ordinary local Pkl amends/import; the bundled schema can be vendored; no remote imports or external resource reads
- **Why:** Deterministic validation with no network or environment dependency

### Release age and windows {#release-timing}

- **Renovate:** Duration strings, schedules and timezone keys
- **Count Patchula:** Nonnegative waitHours plus explicit cron/timezone windows, each time a window names being due on the first run after it; empty windows means unrestricted
- **Why:** Typed units and validated timezones; no natural-language scheduling

### Manager names {#manager-names}

- **Renovate:** Our configs use ecosystem names such as gomod, pip_requirements, custom.regex and managerFilePatterns
- **Count Patchula:** Closed manager identifiers (go, requirements, regex, compose); explicit per-manager file sets; no legacy aliases
- **Why:** Fail on typos and keep the independent vocabulary small

### Branch and commit names {#branch-names}

- **Renovate:** Existing configs choose Renovate names or semanticCommits switches
- **Count Patchula:** Default patchula/ branch prefix and explicit chore(deps): commit prefix, both overridable
- **Why:** Independent identity and predictable messages

### No migration or abandonment policy {#maintenance-policy}

- **Renovate:** Config migration, abandonment and post-update options
- **Count Patchula:** No migration or inferred abandonment policy; lockfile refresh and Go tidy are explicit fields
- **Why:** Pkl-only configuration and deliberate, reviewable maintenance

### One group name {#group-names}

- **Renovate:** Separate groupName and groupSlug fields
- **Count Patchula:** One group string identifies the update group; branch slug derivation belongs to the updater
- **Why:** Avoid two configuration identities for the same grouping intent

### No configuration manager {#configuration-dependencies}

- **Renovate:** The renovate-config manager treats Renovate configuration as package files
- **Count Patchula:** No equivalent manager: the comparison adapter excludes only renovate-config snapshots and summaries
- **Why:** patchula uses Pkl and does not resolve Renovate configuration or presets

The fixed comparison rule `renovate-config-manager` excludes only Renovate's own
`renovate-config` manager before normalization. Triage records the repository,
manager, file/dependency/update counts and this link under `exclusions`, separately
from actionable differences. No config-file contents or preset data are read or
interpreted. All other managers, including unknown names, remain in comparison.
This adapter scope rule also applies to branch upgrade summaries and is independent
of the optional update classification rules in `conformance/departures.json`.

## How the Count runs

### No onboarding pull requests {#no-onboarding}

- **Renovate:** Opens an onboarding PR in repos without config
- **Count Patchula:** Repos without config are skipped
- **Why:** Configs are added deliberately

### Token from the GitHub CLI {#gh-token}

- **Renovate:** The token comes from its own configuration or environment
- **Count Patchula:** `PATCHULA_TOKEN`, `GITHUB_TOKEN`, `GH_TOKEN`, then a signed-in GitHub CLI's github.com token
- **Why:** A local `lookup` or dry run works on a machine already signed in to `gh`, with no token to copy

### One process per batch {#batch-execution}

- **Renovate:** One repository at a time, caches reset per repository
- **Count Patchula:** One process per batch, concurrent repos, batch-wide lookups
- **Why:** Speed

### Incremental release history {#incremental-release-history}

- **Renovate:** Fetches full release and tag lists
- **Count Patchula:** Incremental, newest-first, stops at the first known item
- **Why:** Speed and rate limits

### Reading through the API {#no-clones}

- **Renovate:** Clones each repository
- **Count Patchula:** Reads files through the API; clones only for lockfile tools
- **Why:** Speed

### A JSON report {#json-report}

- **Renovate:** Log lines
- **Count Patchula:** Versioned JSON report
- **Why:** Logs are not an API

### Conditional source refresh {#source-refresh}

- **Renovate:** Histories are exposed through registry/platform responses
- **Count Patchula:** Conditional head probes gate GraphQL; registry bodies use declared freshness
- **Why:** Avoid unnecessary histories and respect actual upstream HTTP capabilities

GraphQL POSTs have no conditional HTTP validators, so a conditional REST head
probe gates incremental GitHub history. Registry responses honor declared
max-age (unless no-cache), including npm/sparse responses with validators. Go proxy list/latest and validator-free OCI tag pages additionally default to a
one-hour local freshness floor, configurable with `--go-freshness-secs` and
`--registry-freshness-secs` (zero restores upstream policy). This can delay new
versions and tag removals within that window. `no-cache` and `no-store` remain
authoritative. Fresh bodies are reused; expired bodies are conditionally validated
where supported. Other registries retain upstream freshness.
Unchanged warm network responses are 304; fresh registries need no request.
Historical changes behind an
unchanged GitHub head require future invalidation/reconciliation. This affects
request strategy, not a confirmed proposal difference, so it has no comparison
exemption.

### Rate-limited registries {#registry-availability}

- **Renovate:** Lookup may fail on quota exhaustion or HTTPS redirects
- **Count Patchula:** Follow bounded HTTPS redirects; reuse rate-limited cached data with stale provenance
- **Why:** Preserve useful results while exposing freshness limits

Registry reads follow at most five HTTPS redirects and strip authorization when
the origin changes. Exhausted Docker Hub `ratelimit-remaining` windows stop queued
requests; HEAD checks retain conditional validators. Rate-limited reads can use
the last cached representation without renewing its freshness. Reports expose
`metadata.stale` as catalog/digest-tag keys with original Unix fetch timestamps,
and emit a warning. A cold miss remains a lookup failure. This availability
behavior has no comparison exemption: stale targets still need a live rerun.

### mise-versions catalogs {#mise-versions-catalogs}

- **Renovate:** Upstream API catalogs
- **Count Patchula:** Public mise-versions release/tag lists for plain GitHub lookups, conditional by ETag; mise tools are listed by mise, which reads the mirror itself; `useVersionsHost = false` restores direct sources for both
- **Why:** Fewer GitHub API reads; authenticated non-registry repos route directly, transient mirror errors may fall back, while 403/404/empty answers do not. Tag commit dates are not release-age evidence. The Count no longer reads the mirror's tool index or tool files: mise does that when it lists a tool, with its own fallbacks.

### Release-age catalog reuse {#catalog-reuse}

- **Renovate:** Revalidate on every run
- **Count Patchula:** Reuse successful complete catalog views for `clamp(waitHours - 1, 1, 24)` hours; zero keeps existing freshness, and checked controls bypass reuse
- **Why:** Recompute all eligibility and held-back decisions at run time; report snapshot fetch time and reuse state. Credentials/source modes stay isolated, quota-stale failures are not promoted, and mutable digests remain independently fresh.

## What gets extracted

### Repeated dependencies {#repeated-dependencies}

- **Renovate:** Manager snapshots can contain repeated dependency names
- **Count Patchula:** Optional occurrence IDs preserve different current values, merging identical rows
- **Why:** Retain all manifest occurrences without ambiguous report identities

The report has optional occurrence IDs for repeated manager/name pairs
with different package/current fields, including multiple action refs and mise
versions. Identical occurrences merge their proposals and locations. The
black-box adapter applies the same normalization, so no comparison exclusion is
needed. Unlabelled duplicate identities remain invalid.

### Kubernetes API versions {#kubernetes-api-versions}

- **Renovate:** Reports apiVersion/kind pairs as kubernetes-api dependencies
- **Count Patchula:** Kubernetes extraction owns workload images; API migrations require a separate schema-aware operation
- **Why:** Changing CRD API versions without migrating their fields is unsafe

The adapter excludes only `kubernetes` rows with datasource `kubernetes-api` and
no proposals. It audits their file/dependency counts and this link in exclusions.
Actual API migration proposals remain visible for triage.

### One owner per YAML image {#yaml-image-ownership}

- **Renovate:** Broad helm-values file patterns can repeat Kubernetes images
- **Count Patchula:** Kubernetes workload images belong to kubernetes; HelmRelease values belong to flux
- **Why:** Apply one manager policy to each semantic image location

The adapter excludes a duplicate `helm-values` Docker row only when the baseline also has
the same package, current fields and proposals in `kubernetes` or `flux` in the
same file, and patchula reports that owner/package there. Different versions,
digests, proposals, files, missing owners and standalone values remain visible.
The same rule recognizes a duplicate `kubernetes` image already owned by `flux`
in a HelmRelease. All removed duplicate rows and proposals are counted in exclusions.

### Indirect Go requirements {#indirect-go-requirements}

- **Renovate:** The compared output skips indirect requirements
- **Count Patchula:** Explicit go.mod requirements are maintained, including indirect ones
- **Why:** Security fixes should not depend on a direct dependency changing

Only extra version upgrades on an existing Go dependency marked `indirect` by
the baseline's observable depType, with equal current values, no baseline
proposals, a strictly newer SemVer target, identical newValue/newVersion and no
digest qualify. Direct requirements, pseudo-version changes and extraction
differences remain actionable. This is independent of proxy caching/freshness.

### Compose test services {#compose-test-services}

- **Renovate:** The compared output omits test/docker-compose.* manifests
- **Count Patchula:** Extract and update explicitly declared test service images
- **Why:** Integration test infrastructure also needs maintenance

Only extra `compose` dependencies in `test/**/docker-compose.*.yml` or `.yaml`,
with Docker datasource and container-image extraction metadata qualify. Unknown
sources, other managers and files outside that explicit test path remain visible.

### Unversioned references {#unversioned-references}

- **Renovate:** Retains unused/unversioned provider, local module and setup-input records
- **Count Patchula:** Reports concrete dependency occurrences that can be looked up or edited
- **Why:** Avoid inventing version locations for references without versions

Only baseline rows missing from patchula, in terraform/github-actions/flux/npm,
with no proposals, no resolved version or digest, and either no current value,
a local `file:`/`./`/`../` reference or a GitHub expression are excluded and audited.
Concrete versions, opaque selectors such as `latest`, and all proposals remain
visible. This does not excuse missing versioned setup-action inputs.

### Installed versions {#current-versions}

- **Renovate:** currentVersion can mean newest satisfying release or floating tag
- **Count Patchula:** Current version follows the lockfile or pinned commit when available
- **Why:** Keep installed/pinned identity separate from candidate resolution

Only current.version mismatches on matching declarations, packages and digests
qualify, with explicit actual resolution metadata equal to its current.version.
For npm lockfiles, both versions must satisfy the same declaration and the
baseline must be newer. For pinned GitHub actions, a 40-digit SHA and a numeric
partial alias are required; both full versions must lie on that alias's line,
with the baseline newer. Changed values, digests, targets, unknown provenance,
unresolved pins and other sources remain actionable. Reports without provenance
cannot use this departure.

### Go language floor {#go-language-floor}

- **Renovate:** Can resolve the go directive as the installed toolchain
- **Count Patchula:** Report the declared minimum language version and its own upgrades
- **Why:** Keep minimum language support distinct from a toolchain installation

A go.mod `go` directive declares a language floor, distinct from an installed
compiler. Narrow rules require the exact go/go identity, baseline
golang-version/golang metadata, actual mise/go-directive metadata, identical
manifest values/digests and explicit declared resolution equal to the floor.
Only a newer baseline compiler and digest-free exact patch/minor floor edits
up to that compiler qualify when the baseline has no proposals. Toolchains,
modules, invented resolutions and targets beyond the observed compiler stay
visible. This explains the three reviewed rows without a generic Go exemption.

### mise backends {#mise-backends}

- **Renovate:** Can select older or different backend identities
- **Count Patchula:** Ask the pinned mise to list each tool (`mise ls-remote --json --prerelease`), in a sandbox, and report the tool's own name as its package with the source `mise`
- **Why:** Every backend mise lists works, with mise's own registry and version rules, without the Count keeping a copy of them

The Count used to read a snapshot of mise's registry and map the first backend
he knew to one of his own sources. That left out the forms mise adds first
(`packslip:` monorepo paths and publisher domains, `go:`, `gem:`, `dotnet:`,
`spm:`, `conda:`, `http:`, `s3:` and more) and could drift from mise. Now the
lookup is mise's, so the report differs from Renovate's for every mise tool:
the package is `prettier`, `bitwarden-secrets-manager` or `jq` rather than the
backend's own repository or registry package, and the source is `mise`. The
comparison rule explains the `package` field only: a dependency listed by mise
whose package is its own name, of kind tool or toolchain, with the same current
value and digest as Renovate's. Proposals and targets are compared separately
and stay visible. This is not package equivalence.

- Branches for mise tools are named after the tool: `patchula/jq-1.x`, not
  `patchula/jqlang-jq-1.x`. A pull request opened under the old name is no
  longer an update he plans, so he treats it as any other update that is no
  longer needed (see [pull requests](/guide/pull-requests/)) and opens the new
  one.
- The snapshot stays for one thing only: telling which GitHub repositories the
  mise-versions mirror lists, for plain GitHub lookups.
- mise's listing carries no tag prefixes, so the Count's version patterns for
  `jq-1.7`, `bun-v1.1`, `go1.22` and `v3_4_11` are gone with the table of
  tools they served.
- A tool installed from a Git ref (`cargo:https://github.com/owner/repo`) lists
  only `HEAD`, so the lookup reports that there are no releases. The Count
  used to read that repository's GitHub tags for it, including for `tag:` pins.
- Tool options (in brackets, or in a table) are not passed to mise. A
  `github:` tool with `version_prefix` or `tag_regex` lists the way mise lists
  it without them. The Count never read these options either. Some options
  redirect a backend to another host (`api_url`, for one) and the lookup
  carries the GitHub token, so passing them needs a list of safe options first.
- Release notes find the repository from the release URL in mise's listing,
  from a table for core tools, or from npm, PyPI and crates.io when mise picks
  one of them as the backend. A tool with none of these, such as a `go:` tool
  or the `java` and `dotnet` core tools, has no release notes.

### mise plugins are never run {#mise-plugins}

- **Renovate:** No equivalence is claimed
- **Count Patchula:** Refuse `asdf:` and `vfox:` tools, and registry names that only have a plugin backend, with a lookup warning
- **Why:** An asdf plugin is shell scripts and a vfox plugin is Lua, both cloned from a repository the dependency file names. Running them would run code from the repository the Count is reading, on the machine that holds his token

Names for the two backends are refused before mise starts. mise itself is told
not to use them (`MISE_DISABLE_BACKENDS=asdf,vfox`), so a registry name such as
`aapt2`, whose only backend is a vfox plugin, fails with mise's error, and a
name that lists a plugin first and another backend later uses the other
backend. Names with bracketed tool options, a leading `-` or whitespace are
refused as well. A backend prefix mise does not know is not checked by the
Count, so a backend mise adds later works; mise fails the lookup for one it lacks.

## Which updates are proposed

### Pins and lockfile refreshes {#update-types}

- **Renovate:** Distinguishes pinDigest and lockFileMaintenance
- **Count Patchula:** Both version/digest additions are pin; lockfile refresh is a separate policy
- **Why:** Keep version classification separate from maintenance

### Cargo compatibility {#cargo-compatibility}

- **Renovate:** Numeric update levels may differ in black-box reports
- **Count Patchula:** A changed pre-1.0 Cargo compatibility boundary is major; exact Cargo uses =
- **Why:** Follow Cargo's documented compatibility rules

Cargo's pre-1.0 breaking boundaries use its documented leftmost-nonzero rule;
npm classifications retain numeric SemVer components. Exact Cargo versions use
`=`. npm widen retains the original range as a union with the target; Cargo
uses a convex interval because Cargo requirements cannot express unions.
Compound preserve edits with no safe operator-preserving representation report
a warning instead of inventing a range. The narrow cargo-pre1-breaking rule
classifies only minor/patch-to-major taxonomy differences at those boundaries,
with identical update targets. It never excludes version or range differences.

### Minimum release age {#minimum-release-age}

- **Renovate:** The compared run waits about three days
- **Count Patchula:** Follow mise's default minimum_release_age of 24 hours for dated releases; choose the newest eligible release and report held-back versions
- **Why:** A short explicit default with release-date evidence; undated releases have no delay

Dated releases wait 24 hours, the same default as mise's
[`minimum_release_age`](https://mise.jdx.dev/configuration/settings.html#minimum_release_age).
A release is dated by its registry publish time. A GitHub tag is dated by its
release's publish time, or else by an annotated tag's date. A lightweight tag
without a release is undated and has no wait. Commit dates never count.

In the comparison runs Renovate held new releases for longer, somewhere between
about 72 and 84 hours, judging from its output alone. A difference is
explained by this departure when:

- the Count's target is newer than Renovate's, in the same update class, and
  between 24 and 96 hours old; or
- the Count is still holding the exact target Renovate proposed, inside its
  24-hour window at the time of the lookup.

For GitHub Actions the matching digest change is explained on the same terms.
Anything else stays a real difference: undated releases, targets older than
the window, or a changed digest for the same version.

### npm range edits {#npm-range-edits}

- **Renovate:** The compared policy can retain caret/tilde ranges
- **Count Patchula:** versions = "exact" pins installed dependencies/devDependencies; peers widen ranges or bump exact values
- **Why:** Reproducible installs while preserving consumer support

The narrow rule requires a major upgrade from a numeric caret/tilde current
range, a patchula exact target with explicit policy, and a baseline caret/tilde
range on the target's major line that contains the selected target. Resolved
targets must be identical, or independently explained by release-age evidence.
Minor/patch range edits additionally qualify only for simple
caret/tilde ranges that contain the identical resolved target, the correct
numeric update class, and explicit exact policy. Extra range pins require an
existing npm dependency, the same caret/tilde current range, no baseline pin, a
satisfying exact target, no digest, npm datasource and explicit exact policy.
Other target, update-type, or range differences stay actionable.

The exact major rule also accepts a lower caret/tilde floor on the target
major line when it contains the selected target, and numeric shorthand current
ranges. Differing targets require independent release-age evidence. An unchanged
satisfying baseline minor/patch declaration paired with the identical exact pin
is also classified. **Peers never qualify for these exact-pin exemptions:**
ranges widen to include the new major (`^5 || ^7.0.0`), exact peers bump in
place, and satisfying peer ranges are never pinned by `versions = "exact"`.

### Pinning floating selectors {#floating-selectors}

- **Renovate:** Leaves some latest selectors without proposals
- **Count Patchula:** Explicit exact policy pins latest to an eligible release
- **Why:** Reproducible toolchains and manifests

Only extra `pin` proposals for matching `mise`/`npm` dependencies (or explicit
GitHub setup-action tool inputs with tool-kind/GitHub-release metadata) whose current
value is exactly `latest`, with no baseline proposals, explicit `exact` policy,
no digest and identical parsed newValue/newVersion qualify. Numeric selectors,
ranges, ordinary upgrades and missing dependencies remain actionable.

### A separate patch track {#patch-track}

- **Renovate:** Often proposes only the highest non-major release
- **Count Patchula:** Offers a patch track alongside a minor track
- **Why:** Let callers choose a smaller upgrade independently

Only an extra patch strictly newer than the baseline's resolved current version,
on the same major/minor line, whose manifest value actually changes, qualifies.
Normally both inputs must contain the identical newer minor target/value/digest on
that major line, and the patch must carry explicit policy metadata. Missing
targets, old/unchanged patches, no-op Cargo ranges, other classes and absent minor
alternatives remain actionable.

The minor alternative may differ only when the existing release-age predicate
explains its target, or be absent only when that exact baseline minor is held
inside an evidenced 24-hour window at evaluation. npm integrity and Helm archive
checksums are ignored consistently here as in the main comparison. Different
package/current declarations, expired holds and unexplained minor targets remain
actionable.

### Go major versions {#go-major-versions}

- **Renovate:** Can report a new numeric major under the old module name
- **Count Patchula:** Go proxy catalogs preserve module identity; /vN migrations are separate
- **Why:** A version-only edit cannot update module paths and imports

Only missing `major` proposals on existing GitHub-hosted Go modules qualify,
with matching module/current identities and Go datasource on both sides. The
target must be a stable, build-metadata-free SemVer major >= 2, greater than
current, with identical newValue/newVersion and no digest, while the module path
lacks its required `/vN` suffix. `+incompatible` releases, other hosts, correct
target module paths and digest changes remain actionable. Automatic module-path
and import migration is future work; this rule does not claim it is implemented.

### Cargo no-op proposals {#cargo-no-ops}

- **Renovate:** Can propose a newer satisfying release with unchanged newValue
- **Count Patchula:** Suppress proposals that cannot edit the manifest
- **Why:** Lock refreshes need an explicit future lockfile operation

Only missing version proposals on an existing Cargo dependency with identical
package and current declarations, baseline `crate` and actual `crates` sources,
an unchanged newValue, no digest, and a stable target satisfying the original
non-exact Cargo requirement qualify. Changed ranges, exact `=` requirements,
unsatisfied targets, digest changes, other sources and missing dependencies stay
actionable. A future explicit lockfile refresh can maintain satisfying releases;
the current lookup does not claim to implement it.

### Disabled update types {#disabled-update-types}

- **Renovate:** Lookup snapshots can contain targets disabled by repository rules
- **Count Patchula:** Report effective disabled classes and omit their proposals
- **Why:** Preserve repository policy during comparisons

Only missing proposals on matching package/current identities with a valid
`metadata.lookup.evaluatedAt` and the exact proposal class in `disabledUpdates`
qualify. No inference is made from absent proposals or old reports lacking policy
evidence. Rules are evaluated in order using the dependency's real file and kind.

### Deprecated and yanked releases {#rejected-releases}

- **Renovate:** Lookup can propose deprecated releases
- **Count Patchula:** Reject deprecated/yanked versions and record the exact rejected targets
- **Why:** Avoid moving users onto withdrawn releases

Only missing version upgrades without digests on matching package/current
identities qualify, with an npm/crates/PyPI actual source and a valid evaluation
timestamp. The exact proposed version must appear as deprecated or yanked in
`metadata.lookup.rejected`. Other targets, digest changes, unknown reasons and
reports without rejection evidence stay actionable.

### Stable releases, not LTS {#release-channels}

- **Renovate:** Some Node targets follow the LTS line
- **Count Patchula:** Select the newest eligible stable version allowed by explicit policy
- **Why:** Do not infer a hidden LTS restriction from a package name

Newest eligible stable remains the default. The reviewed Node differences
(mise Node, setup-node inputs, and @types/node) select a newer stable major
while the observed baseline stays on a lower line. Narrow rules require these
specific source identities, equal declarations/digests, explicit stable policy
(or the legacy default), stable ordered majors and digest-free major proposals.
Other packages, peer ranges, same-major target drift and explicit LTS policy do
not qualify. This explains the observed channel choice without guessing LTS
membership from an even major number. For an opt-in Node LTS rule, see
[lookup policy](/guide/lookup-policy/#node-release-channels).

### Action ref precision {#action-ref-precision}

- **Renovate:** Can expand a ref when adding a digest
- **Count Patchula:** Preserve the declared numeric ref precision and report the full resolved target separately
- **Why:** The commit pin supplies reproducibility while the ref describes the intended release line

Adding or updating a commit pin retains a numeric alias such as v4 while
newVersion reports the full target. The narrow rule requires equal package and
current identity, github-tags action/workflow metadata, an unchanged alias with
one or two numeric components, and the identical full target. The actual commit
must be a 40-digit hash and match a baseline commit when supplied; an added
commit is separately audited under [explicit defaults](#explicit-defaults). Changed targets, supplied hashes,
upgrade classes and absent full versions remain visible. The previous newVersion
bug is fixed separately.

For release upgrades retaining a declared major/minor ref's precision, both
sides must propose the same floating tag and update class, with stable full
versions on that line and patchula's target newer. The proposed commit must
equal independent `metadata.tagDigests` evidence for the floating tag, emitted
from the observed peeled tag catalog. This can explain both full-version and
digest differences when Renovate pins an older release tag's commit. Missing
evidence, changed lines/precision and unrelated hashes remain actionable.

### Cargo target resolution {#cargo-target-resolution}

- **Renovate:** Observed pre-1.0 upgrades can select an older release in the proposed range
- **Count Patchula:** Resolve the newest eligible release in that same compatibility interval
- **Why:** Report the available release without narrowing the manifest

The fresh output resolves keepass 0.15.0 versus 0.15.2, and toml_edit 0.25.16
versus 0.25.17+spec-1.1.0. All exceed both release-age windows. Both sides propose
the identical bare two-component Cargo interval, which contains both targets;
patchula selects the newest eligible release available from its sparse index.
This is a resolved-target choice, not capture skew or a range-floor exemption.
The narrow rule requires matching package/current declarations and digests,
crate/crates sources, no stale evidence, a current pre-1.0 minor below both
targets, ordered stable releases on the same nonzero pre-1.0 minor, identical
proposed `0.minor` intervals, explicit preserve policy, no proposed digests,
and the observed minor-to-major Cargo taxonomy. Both missing/extra rows remain
separately audited. Changed ranges, different compatibility intervals, stale
lookups and unrelated taxonomy remain actionable. Tests cover negative cases.

### Reference data {#reference-data}

- **Renovate:** No built-in category; release age, schedules and automerge apply as configured
- **Count Patchula:** CA root bundles, time zone data and the public suffix list are `reference-data`; by default their updates skip the wait and windows, get their own PR and merge once checks pass
- **Why:** A stale trust store keeps trusting a distrusted CA, and these packages change data, not APIs

See [reference data](/guide/configuration/#reference-data) for the packages and
how to turn it off.

### Advisories from OSV.dev {#advisories}

- **Renovate:** `vulnerabilityAlerts` reads the repository's GitHub Dependabot alerts; OSV is a separate opt-in
- **Count Patchula:** OSV.dev only, on by default, one batched query per run; fixing updates are marked and labelled, not given separate alert PRs
- **Why:** Works without the App's `vulnerability_alerts` permission or Dependabot being enabled, covers RustSec and the Go database, and fits one lookup per batch

## Lockfiles and digests

### npm integrity {#npm-integrity}

- **Renovate:** Lookup reports npm integrity as newDigest
- **Count Patchula:** npm manifests do not pin integrity; compare targets without npm newDigest
- **Why:** Lockfile integrity belongs to lockfile tooling

The adapter ignores only proposed newDigest for dependencies whose datasource is
npm (falling back to npm/bun manager when source metadata is absent), including
custom regex extraction of npm versions. Current digests, other sources, version
targets and update types remain comparable. Triage counts normalized proposals.

### Helm archive checksums {#helm-archive-checksums}

- **Renovate:** Lookup reports chart archive hashes as newDigest
- **Count Patchula:** Helm chart references contain versions, not archive hashes
- **Why:** Artifact verification metadata does not edit the manifest

Only proposed `newDigest` with datasource `helm` is ignored, with a normalization
count. Current digests, OCI/Docker digests and all version targets remain compared.
Helm registry URLs in observable output are joined to chart names to compare the
same registry identity used by patchula; a different registry stays actionable.

Comparison representation rules also canonicalize Bun's npm-sourced (or
datasource-unspecified)
`package.json` rows to `npm` (same manifest/dependency identity), mise version
`v` prefixes, and Docker numeric `version` fields while retaining full variant
tags in `value`. Regex GitHub versions and Flux chart versions also canonicalize
numeric `v` prefixes while preserving the literal current/proposed value.
An absent baseline currentVersion is unknown, not an assertion
of an unresolved actual version. Redundant absent newVersion on an otherwise
identical digest proposal is also unknown. Each normalization is counted; value,
digest and target differences remain visible. A missing/invalid baseline is
reported separately from dependency differences and never implies parity.

### aube for npm and pnpm {#node-lockfile-engine}

- **Renovate:** Regenerate npm and pnpm locks with npm and pnpm
- **Count Patchula:** Regenerate `pnpm-lock.yaml`, `package-lock.json` and `npm-shrinkwrap.json` with aube, which writes the committed format in place; `lockfiles.npmEngine = "npm"` restores native npm for npm lockfiles only
- **Why:** Much faster lockfile updates; aube's npm lockfiles stay valid for `npm ci` but may differ in hoisting and `funding` metadata from npm's own output

patchula regenerates `pnpm-lock.yaml`, `package-lock.json` and
`npm-shrinkwrap.json` with aube instead of pnpm and npm, and regenerates
`yarn.lock` with Yarn itself, Classic and Berry. aube reads and writes the
committed format in place and is told the committed lockfile name, so it never
introduces `aube-lock.yaml` and a second lockfile in the same directory cannot
redirect it. The departure is the lockfile content aube produces, not the
resolved versions: aube's npm lockfile stays valid for `npm ci`, but it may
differ from npm's own output in hoisting and in the shape of `funding`
metadata, and a later `npm install` rewrites those lines. `lockfiles.npmEngine
= "npm"` keeps native npm for npm lockfiles; pnpm always uses aube and Yarn
always uses Yarn, because aube's Yarn Berry output is rejected by Yarn.
Native npm also regenerates format v1 npm lockfiles, which aube cannot read.
Trees with local `file:` dependencies stay with aube, which keeps their
entries valid for `npm ci` since 2.8.1
([aube#1796](https://github.com/aubepkg/aube/issues/1796)). A
lockfile that does not change after a manifest edit that changes recorded
dependencies fails the branch, so a tool that reports "up to date" without
re-resolving cannot publish a stale lock. This entry grants no lookup
comparison exemption.

## Pull requests and release notes

### Branch and merge safety {#write-safety}

- **Renovate:** No write behavior is inferred from comparison output
- **Count Patchula:** Leased API refs, verified bot ownership, receipt-backed later-run merges when native auto-merge is unavailable
- **Why:** Preserve human work and lockfile consistency without emulating another bot

The Count makes no claim to match Renovate's branch and commit behaviour. A
branch is identified by its source and package, or its exact group, plus the
update class, with a stable hashed suffix. Each update is one API commit on the
chosen base, signed by GitHub; unverified objects are never published. Branches
carrying human, unmarked or unverified commits are protected. Atomic ref leases
guard the base and the update branch against concurrent changes. Lockfiles are
regenerated in the same commit; a lock the Count cannot regenerate holds the
branch as lockfile-pending. This section exempts no lookup difference. See the
[branch actions](/reference/report/#actions) in the JSON report.

Branch names follow the observed `<dependency>-<major>.x`, shared pins and
`major-<group>` forms. Container variants keep the major and the variant
(`3.x-slim`), and manager-prefixed slug collisions need explicit groups. See
[branches and PRs](/guide/pull-requests/).

This departure also covers the independently specified [auto-merge fallback](/guide/auto-merge/).
The supplied black-box comparisons do not exercise Renovate merging PRs, so no
observed merge-behavior difference or new conformance exemption is claimed.
The explicit patchula policy requires a prior successful head receipt, at least
one successful check/status, clean GitHub mergeability, and current release-age
eligibility; a manual creation permit does not waive the merge gates. Schedule
windows only decide when updates are proposed.

### Release notes {#release-notes}

- **Renovate:** No presentation compatibility is claimed
- **Count Patchula:** Ecosystem metadata, public mise-versions note lists, cached changelog sections, bounded ranges, prose-only mention neutralisation and readable references through GitHub's documented backlink-free redirect host
- **Why:** Useful upstream context without steady-run reads or notification/timeline noise; see [release notes](/guide/release-notes/)
