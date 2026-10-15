# Deliberate departures from Renovate

rustovate is not a Renovate clone. Each entry records a behaviour that differs
on purpose, and why. The comparison harness links differences it finds to
entries here; a difference with no entry is treated as a bug.

Review map (45 entries; IDs below remain stable):

- **Policy:** 2–8, 10, 12, 14, 16–18, 20, 23, 25, 29–31, 33–35,
  37–38, 40–43, 45 cover explicit activation, reproducibility, update tracks,
  release timing and editable declarations. rustovate is developed independently;
  the twelve public repositories are test inputs, not product requirements.
- **Performance and strategy:** 21 and 36 describe conditional history probes,
  local freshness and quota fallbacks. They grant no comparison exemption.
- **Model and representation:** 1, 9, 11, 13, 15, 19, 22, 26–27, 32, 39,
  43–45 describe typed configuration, ownership and observed identity.
- **Comparison-only normalization:** 24 and 28 remove lock/archive verification
  fields from manifest comparisons; 19, 26 and 27 also scope/audit duplicate or
  uneditable inventory. Numeric prefixes and missing optional fields are counted
  normalizations, not extra product policies.

**Questionable choices for owner review:** 21's one-hour catalog floor can delay
new releases; 23 uses commit dates for GitHub tags, which cannot prove creation
time for lightweight tags; 25's exact peer pins can narrow a library's support
contract; 40's higher Cargo floor and 41's exact provider pins trade breadth for
explicit selection; 42 chooses stable releases without inferring LTS. Configure
support ranges/channels explicitly where these tradeoffs are unsuitable. These
are policy choices, not evidence that a stale or failed lookup is equivalent.

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
| 25 | npm range edits | The compared policy can retain caret/tilde ranges | versions = "exact" proposes exact releases and range pins | Apply explicit version policy consistently |
| 26 | Kubernetes API inventory | Reports apiVersion/kind pairs as kubernetes-api dependencies | Kubernetes extraction owns workload images; API migrations require a separate schema-aware operation | Changing CRD API versions without migrating their fields is unsafe |
| 27 | YAML image ownership | Broad helm-values file patterns can repeat Kubernetes images | Kubernetes workload images belong to kubernetes; HelmRelease values belong to flux | Apply one manager policy to each semantic image location |
| 28 | Helm archive checksums | Lookup reports chart archive hashes as newDigest | Helm chart references contain versions, not archive hashes | Artifact verification metadata does not edit the manifest |
| 29 | Floating tool and npm selectors | Leaves some latest selectors without proposals | Explicit exact policy pins latest to an eligible release | Reproducible toolchains and manifests |
| 30 | Indirect Go dependencies | The compared output skips indirect requirements | Explicit go.mod requirements are maintained, including indirect ones | Security fixes should not depend on a direct dependency changing |
| 31 | Compose test services | The compared output omits test/docker-compose.* manifests | Extract and update explicitly declared test service images | Integration test infrastructure also needs maintenance |
| 32 | Unversioned inventory | Retains unused/unversioned provider, local module and setup-input records | Reports concrete dependency occurrences that can be looked up or edited | Avoid inventing version locations for references without versions |
| 33 | Patch alternatives | Often proposes only the highest non-major release | Offers a patch track alongside a minor track | Let callers choose a smaller upgrade independently |
| 34 | Go module identity | Can report a new numeric major under the old module name | Go proxy catalogs preserve module identity; /vN migrations are separate | A version-only edit cannot update module paths and imports |
| 35 | Cargo manifest no-ops | Can propose a newer satisfying release with unchanged newValue | Suppress proposals that cannot edit the manifest | Lock refreshes need an explicit future lockfile operation |
| 36 | Registry availability | Lookup may fail on quota exhaustion or HTTPS redirects | Follow bounded HTTPS redirects; reuse rate-limited cached data with stale provenance | Preserve useful results while exposing freshness limits |
| 37 | Explicit disabled updates | Lookup snapshots can contain targets disabled by repository rules | Report effective disabled classes and omit their proposals | Preserve repository policy during comparisons |
| 38 | Rejected releases | Lookup can propose deprecated releases | Reject deprecated/yanked versions and record the exact rejected targets | Avoid moving users onto withdrawn releases |
| 39 | Observed current versions | currentVersion can mean newest satisfying release or floating tag | Current version follows the lockfile or pinned commit when available | Keep installed/pinned identity separate from candidate resolution |
| 40 | Cargo range floors | Can retain a lower compatibility floor such as 1.0 for target 1.1.0 | Preserve the declaration's precision while moving its floor to the selected release, such as 1.1 | Do not claim compatibility with releases below the selected target line |
| 41 | Exact Terraform provider pins | Can retain a satisfying provider constraint without a manifest edit | Explicit exact policy replaces it with the selected version | A reproducible provider selection should be an intentional manifest edit |
| 42 | Release channels | Some Node targets follow the LTS line | Select the newest eligible stable version allowed by explicit policy | Do not infer a hidden LTS restriction from a package name |
| 43 | Go language floor | Can resolve the go directive as the installed toolchain | Report the declared minimum language version and its own upgrades | Keep minimum language support distinct from a toolchain installation |
| 44 | mise registry identity | Can select older or different backend identities | Follow the first supported backend in the published mise registry snapshot | Use the tool's current published backend; never equate distinct package identities |
| 45 | Action alias precision | Can expand a ref when adding a digest | Preserve the declared numeric ref precision and report the full resolved target separately | The commit pin supplies reproducibility while the ref describes the intended release line |

<a id="departure-42"></a>**Departure 42: explicit stable channels.**
The reviewed live cases are Node major targets in mise/setup-node and the
corresponding npm type package: stable 26 versus the baseline's 24 line.
They are not a one-hour age crossing. An explicit permitted filter can keep an
LTS line. **Review choice:** newest stable is not always the desired production
channel. No automatic comparison rule guesses LTS membership from a name or
major number; these eight rows stay visible pending a channel decision.

<a id="departure-43"></a>**Departure 43: Go language directives.**
The reviewed case declares `go 1.26` while the baseline resolves currentVersion
to 1.27.2 and emits no go-directive upgrades. rustovate retains 1.26 and offers
1.26.9/1.27.2 as explicit edits to that floor. This does not infer an installed
compiler from go.mod. The three rows remain visible: there is no general Go
currentVersion exemption, and toolchain/module dependencies are unaffected.

<a id="departure-44"></a>**Departure 44: published mise backends.**
The reviewed names are bitwarden-secrets-manager (`bitwarden/sdk-sm` rather
than `bitwarden/sdk`) and prettier (npm `prettier` rather than GitHub
`prettier/prettier`). Backend identity is observable and remains compared.
The resolver's public registry snapshot and product tag-prefix mappings are
independent of the test repositories. There is no package-equivalence exemption;
explicit backend selectors can choose a different source.

<a id="departure-45"></a>**Departure 45: action pin precision.**
For an unpinned numeric alias such as v4, adding a commit keeps newValue v4 and
reports the full version (for example v4.4.0) in newVersion. The digest must be
the alias's peeled commit. A digest update on an existing pin resolves the new
commit, not the old pin. The supplied report's v4/v2 newVersion fields were a
bug, now fixed; the ref-value choice is intentional. No exemption hides those
old report fields or a changed hash.

<a id="departure-40"></a>**Departure 40: Cargo target range floors.**
Only paired Cargo proposals with identical package/current identities, target,
class and no digest qualify. The current declaration must be a bare two-component
range, actual source must be crates, and explicit preserve policy must produce
the target's major/minor line. The baseline range must contain that exact target.
Target, precision, policy, digest and package changes remain actionable. This
does not classify the older keepass/toml_edit targets in the live comparison.

<a id="departure-41"></a>**Departure 41: exact provider pins.**
Only same-target, same-class, digest-free provider updates with matching
package/current identities qualify. Actual metadata must say terraform/provider
and carry explicit exact policy. The baseline must leave the current `~> x.y`
or `~> x.y.z` range unchanged, and that range must contain the stable target;
actual newValue must equal newVersion. Module/runtime constraints, changed
targets or ranges and absent policy stay actionable. **Review choice:** exact
pins sacrifice the original support interval; use preserve policy for reusable
modules that intentionally support a range.

<a id="departure-37"></a>**Departure 37: effective disabled classes.**
Only missing proposals on matching package/current identities with a valid
`metadata.lookup.evaluatedAt` and the exact proposal class in `disabledUpdates`
qualify. No inference is made from absent proposals or old reports lacking policy
evidence. Rules are evaluated in order using the dependency's real file and kind.

<a id="departure-38"></a>**Departure 38: rejected release evidence.**
Only missing version upgrades without digests on matching package/current
identities qualify, with an npm/crates/PyPI actual source and a valid evaluation
timestamp. The exact proposed version must appear as deprecated or yanked in
`metadata.lookup.rejected`. Other targets, digest changes, unknown reasons and
reports without rejection evidence stay actionable.

<a id="departure-39"></a>**Departure 39: current resolution provenance.**
Only current.version mismatches on matching declarations, packages and digests
qualify, with explicit actual resolution metadata equal to its current.version.
For npm lockfiles, both versions must satisfy the same declaration and the
baseline must be newer. For pinned GitHub actions, a 40-digit SHA and a numeric
partial alias are required; both full versions must lie on that alias's line,
with the baseline newer. Changed values, digests, targets, unknown provenance,
unresolved pins and other sources remain actionable. Reports without provenance
cannot use this departure.

<a id="departure-35"></a>**Departure 35: Cargo manifest no-ops.**
Only missing version proposals on an existing Cargo dependency with identical
package and current declarations, baseline `crate` and actual `crates` sources,
an unchanged newValue, no digest, and a stable target satisfying the original
non-exact Cargo requirement qualify. Changed ranges, exact `=` requirements,
unsatisfied targets, digest changes, other sources and missing dependencies stay
actionable. A future explicit lockfile refresh can maintain satisfying releases;
the current lookup does not claim to implement it.

<a id="departure-36"></a>**Departure 36: rate-limited registry cache.**
Registry reads follow at most five HTTPS redirects and strip authorization when
the origin changes. Exhausted Docker Hub `ratelimit-remaining` windows stop queued
requests; HEAD checks retain conditional validators. Rate-limited reads can use
the last cached representation without renewing its freshness. Reports expose
`metadata.stale` as catalog/digest-tag keys with original Unix fetch timestamps,
and emit a warning. A cold miss remains a lookup failure. This availability
behavior has no comparison exemption: stale targets still need a live rerun.

<a id="departure-34"></a>**Departure 34: Go major module paths.**
Only missing `major` proposals on existing GitHub-hosted Go modules qualify,
with matching module/current identities and Go datasource on both sides. The
target must be a stable, build-metadata-free SemVer major >= 2, greater than
current, with identical newValue/newVersion and no digest, while the module path
lacks its required `/vN` suffix. `+incompatible` releases, other hosts, correct
target module paths and digest changes remain actionable. Automatic module-path
and import migration is future work; this rule does not claim it is implemented.

<a id="departure-33"></a>**Departure 33: separate patch track.**
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

<a id="departure-32"></a>**Departure 32: unversioned inventory.**
Only baseline rows missing from rustovate, in terraform/github-actions/flux/npm,
with no proposals, no resolved version or digest, and either no current value,
a local `file:`/`./`/`../` reference or a GitHub expression are excluded and audited.
Concrete versions, opaque selectors such as `latest`, and all proposals remain
visible. This does not excuse missing versioned setup-action inputs.

<a id="departure-29"></a>**Departure 29: exact latest pins.**
Only extra `pin` proposals for matching `mise`/`npm` dependencies (or explicit
GitHub setup-action tool inputs with tool-kind/GitHub-release metadata) whose current
value is exactly `latest`, with no baseline proposals, explicit `exact` policy,
no digest and identical parsed newValue/newVersion qualify. Numeric selectors,
ranges, ordinary upgrades and missing dependencies remain actionable.

<a id="departure-30"></a>**Departure 30: indirect Go upgrades.**
Only extra version upgrades on an existing Go dependency marked `indirect` by
the baseline's observable depType, with equal current values, no baseline
proposals, a strictly newer SemVer target, identical newValue/newVersion and no
digest qualify. Direct requirements, pseudo-version changes and extraction
differences remain actionable. This is independent of proxy caching/freshness.

<a id="departure-31"></a>**Departure 31: Compose test services.**
Only extra `compose` dependencies in `test/**/docker-compose.*.yml` or `.yaml`,
with Docker datasource and container-image extraction metadata qualify. Unknown
sources, other managers and files outside that explicit test path remain visible.

<a id="departure-26"></a>**Departure 26: Kubernetes API inventory.**
The adapter excludes only `kubernetes` rows with datasource `kubernetes-api` and
no proposals. It audits their file/dependency counts and this link in exclusions.
Actual API migration proposals remain visible for triage.

<a id="departure-27"></a>**Departure 27: one YAML image owner.**
The adapter excludes a duplicate `helm-values` Docker row only when the baseline also has
the same package, current fields and proposals in `kubernetes` or `flux` in the
same file, and rustovate reports that owner/package there. Different versions,
digests, proposals, files, missing owners and standalone values remain visible.
The same rule recognizes a duplicate `kubernetes` image already owned by `flux`
in a HelmRelease. All removed duplicate rows and proposals are counted in exclusions.

<a id="departure-28"></a>**Departure 28: Helm archive checksums.**
Only proposed `newDigest` with datasource `helm` is ignored, with a normalization
count. Current digests, OCI/Docker digests and all version targets remain compared.
Helm registry URLs in observable output are joined to chart names to compare the
same registry identity used by rustovate; a different registry stays actionable.

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

<a id="departure-23"></a>**Departure 23: minimum release age.**
The default follows [mise's minimum_release_age](https://mise.jdx.dev/configuration/settings.html#minimum_release_age):
24 hours when a release timestamp is available. The comparison rule requires an
explicit 24-hour policy and timestamp evidence that rustovate's newer proposal is
at least 24 and less than 72 hours old, plus a strictly older Renovate target in
the same update class. A newer baseline target instead requires an unexpired
24-hour held-target record. Undated, older-than-window and unrelated target
differences remain actionable. This is a narrow explanation of
the observed three-day comparison window, not a claim about all Renovate defaults.
New reports additionally allow a missing exact target to be classified when
`metadata.lookup.evaluatedAt` falls inside that target's recorded 24-hour
held-back window. Only matching package/current identities, digest-free version
upgrades and the exact summary version/class qualify. Missing evaluation evidence,
elapsed windows and fallback extras remain actionable.

For action tag upgrades only, the same 24–72-hour newer-target explanation can
also classify the paired digest difference: identical package/current identity,
same numeric alias and upgrade class, both 40-digit commit hashes, explicit
24-hour policy and distinct ordered full target versions are required. A changed
hash for the same target version remains actionable. This is a policy-window
departure, not evidence that the baseline's one-hour capture skew caused it.

<a id="departure-24"></a>**Departure 24: npm integrity normalization.**
The adapter ignores only proposed newDigest for dependencies whose datasource is
npm (falling back to npm/bun manager when source metadata is absent), including
custom regex extraction of npm versions. Current digests, other sources, version
targets and update types remain comparable. Triage counts normalized proposals.

<a id="departure-25"></a>**Departure 25: exact npm major proposals.**
The narrow rule requires a major upgrade from a numeric caret/tilde current
range, a rustovate exact target with explicit policy, and a baseline caret/tilde
range on the target's major line that contains the selected target. Resolved
targets must be identical, or independently explained by release-age evidence.
Minor/patch range edits additionally qualify only for simple
caret/tilde ranges that contain the identical resolved target, the correct
numeric update class, and explicit exact policy. Extra range pins require an
existing npm dependency, the same caret/tilde current range, no baseline pin, a
satisfying exact target, no digest, npm datasource and explicit exact policy.
Other target, update-type, or range differences stay actionable.

The exact major rule also accepts a lower caret/tilde floor on the target's
major line when it contains the selected target, numeric shorthand current
ranges, and a peer-only `current || target-range` widening. Differing targets
require the independent release-age predicate's clock/held-target evidence.
An unchanged satisfying baseline minor/patch declaration paired with the
identical exact pin is also classified; arbitrary missing version targets are
not. Exact peer policy is explicitly flagged for owner review above.

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

<a id="departure-22"></a>**Departure 22: repeated dependency identities.**
The v1 report now has optional occurrence IDs for repeated manager/name pairs
with different package/current fields, including multiple action refs and mise
versions. Identical occurrences merge their proposals and locations. The
black-box adapter applies the same normalization, so no comparison exclusion is
needed. Unlabelled duplicate identities remain invalid.
