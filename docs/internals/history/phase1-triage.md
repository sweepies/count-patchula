# Phase 1 comparison follow-up

## Final live check (2026-10-15): Phase 1 exit met

The supplied fresh release batch took **37.4 s cold / 4.1 s warm / 3.8 s
warm2**, with **343 proposals on all three runs**. Renovate's dry run took
**261 s**; its run started about four minutes before patchula's cold run.
These are caller-supplied single observations. See [benchmarks](/internals/history/benchmarks#phase-1-final-live-check-2026-10-15).

Input triage was **0 missing, 0 extra, 6 field-mismatch, 275 departures**.
Replay against `/root/rustovate-conf/renovate12.jsonl` now gives **0 missing,
0 extra, 0 field-mismatch, 281 departures**, at
`/root/rustovate-conf/out12-closed/triage.{md,json}`. **0 actionable rows: Phase 1 exit is met for the eleven comparable repositories.**

Departure 45 now covers a pinned action whose declared major/minor precision
is retained in the proposed floating tag, while patchula reports a newer
stable release on that same line. It requires matching declarations and update
classes, equal proposed aliases, ordered full versions, and independent
`metadata.tagDigests` evidence that the proposed digest is the alias's peeled
commit. The apple-actions/import-codesign-certs version and digest fields
qualify: v7 and v7.1.0 resolve to `77ba2a2a…`, whereas v7.0.0 resolves to
`5142e029…`. Future engine reports emit this evidence from their tag catalogs.

The original cold capture lacked that additional metadata. Its original bytes
are preserved at `/root/rustovate-conf/r12-cold.original.json`; only the observed
v7 tag evidence was added to `r12-cold.json`, with every original proposal and
timing retained. Evidence comes from the captured `r12.sqlite` matching-refs
response and peeled tag catalog; its extraction is saved as
`/root/rustovate-conf/r12-floating-tag-evidence.json`. No live data was invented
or substituted for the captured proposal set.

Departure 23 measures the newer patchula release's age at the timestamp
of Renovate's own `packageFiles with updates` log record, rather than at
patchula's `evaluatedAt`. Its upper bound is now **96 hours exclusive**, an
empirical comparison ceiling; the explicit 24-hour policy, timestamp evidence,
strictly older Renovate target and matching update class remain required.
Missing/malformed timestamps and ages outside the window remain actionable.
Regression coverage includes the observed 72-hour-17-minute holdback and the
96-hour cutoff between independent evaluations.

The last four fields were unocss and @unocss/reset, 66.10.5 versus 66.11.0.
The original capture was already beyond 72 hours (publication
2026-10-12T06:41:36.594Z; clips snapshot 2026-10-15T06:43:43.844Z).
The clips-only Renovate rerun at **2026-10-15T06:59**, captured in
`/root/rustovate-conf/renovate-clips.jsonl`, still held 66.11.0 at **72 hours
17 minutes** and proposed 66.10.5 for both packages. 66.11.0 was
`dist-tags.latest` and was not deprecated. Earlier runs held effect 4.0.4 at
about 49 hours and renovate 44.155.8 at about 33 hours, but proposed effect
4.0.3 at about 84 hours. The observed holdback window therefore lies between
72 hours 17 minutes and about 84 hours. The 96-hour ceiling covers that evidence;
Renovate's exact rule remains unknown from output alone, and its source was not
read. The earlier 71.9-hour explanation was incorrect.

Coverage remains **eleven comparable repositories and one gap**. remote-agent
has no Renovate baseline because its `renovate.json` is invalid; patchula's
own configuration for that repository validates. This gap is not parity.

Validation: `mise run ci` passed (cassette secret scan, formatting,
workspace/all-target Clippy with `-D warnings`, workspace tests and build).
The HTTP tracing capture test now runs in its own integration-test process:
concurrent untraced HTTP tests could register shared callsites with the
no-subscriber dispatch while the scoped capture subscriber was installed,
caching disabled interest. Isolation removes that registration race while
retaining all credential-redaction assertions. Logs:
`/root/rustovate-conf/last-phase1-ci.log` and
`/root/rustovate-conf/last-phase1-conformance.log`. No push was performed.

## Round 4: fresh baseline, owner decisions and unchanged-report replay

Implementation: `9271512` (local; not pushed).

The supplied release batch at `1826e32`, twelve repositories in one process,
took **33.5 s cold / 2.9 s warm / 3.5 s warm2**, with **344 proposals on all
three passes**. The fresh Renovate dry run took **296 s** and was captured within
about ten minutes of patchula. These are caller-supplied single observations,
not measurements of the round 4 implementation. Proposal determinism is confirmed
for this capture. See [benchmarks](/internals/history/benchmarks#phase-1-exit-round-4-input-2026-10-15).

Fresh input triage: **2 missing, 6 extra, 13 field-mismatch, 264 departures**.
Final replay of the unchanged cold report against the **fresh**
`/root/rustovate-conf/renovate12.jsonl`: **0 missing, 1 extra, 7 field-mismatch,
277 departures**, at `/root/rustovate-conf/out12-r4-final/triage.{md,json}`.
Every original actionable row is accounted for below: 19 have narrow deliberate
departure rules, and two expose the corrected GitHub date bug. Removing the
obsolete Cargo/provider/peer exemptions exposes six additional old-report rows.
Those eight rows intentionally remain actionable in this immutable capture;
no exemption hides a fixed implementation bug.

### SWE-103 owner decisions

- **23:** GitHub tags use a matching release's published date, then an annotated
  tag's tagger date; otherwise the tag is undated and has no wait. Commit dates
  are never used. Versioned object/catalog caches discard old commit dates,
  and late release publication revalidates independently of unchanged tags.
- **25:** Peer ranges widen to include the new major (`^5 || ^7.0.0`), exact
  peers bump in place, and exact policy never pins a peer range. Installed
  dependencies/devDependencies retain exact policy. Peer pin exemptions are gone.
- **42:** Newest eligible stable remains the default. Typed policy/rules now
  support explicit Node `channel = "lts"` using the Node distribution index's
  LTS metadata. See the [configuration example](/guide/lookup-policy#node-release-channels).
- **21:** One-hour catalog freshness remains unchanged and grants no exemption.
- **40/41 removed:** Cargo preserve retains satisfying floors and starts bare
  two-component numeric-major upgrades at the major floor (`1.0` for target
  `1.1.0`). Satisfying Terraform provider constraints remain unchanged, including
  under exact policy. Both departures and their harness predicates are removed.

### All 21 original actionable rows

Row numbers refer to the fresh input's non-departure rows in order. Each row
appears once; product policies remain independent of these twelve test inputs.

| Fresh rows | Disposition | Item 1 relationship |
|---|---|---|
| 1–4, 13–16 | Departure 42: mise Node, setup-node and @types/node choose a newer stable major than the observed baseline line. Guard source identities, declarations, major order, digest absence and stable policy; explicit LTS, peers and same-major drift do not qualify. | Owner retains stable; explicit LTS rule added. |
| 5–6 | apple-actions v7.1.0/version+commit: fix tag release-age provenance. The captured Oct 9 commit date is not evidence of when the tag/release appeared. Retain both fields for a live rerun using release/tagger metadata; do not call it time skew or an accepted target difference. | Directly affected by decision 23. |
| 7–10 | New departure 46: keepass and toml_edit select newer resolved releases within the identical proposed pre-1.0 Cargo compatibility interval. Both exceed the age window. Equal range, stable ordered targets, preserve policy, sources, unchanged current identity and absence of stale/digest evidence are required. | Separate from removed floor departure 40; no changed manifest floor is exempted. |
| 11–12 | Departure 44: the published mise registry selects bitwarden/sdk-sm and npm prettier. Retain distinct package identities; classify only the two observed backend migrations with matching latest declarations, no proposals and exact old/new source metadata. | Independent of owner decisions. |
| 17–19 | Departure 43: declared Go language floor versus observed compiler resolution. Require go/go directive identity, declared provenance, matching declarations and digest-free exact floor updates no newer than the baseline compiler. | Independent; no generic Go-version exemption. |
| 20–21 | Departure 45: preserve v4/v2 action alias precision when pinning. Require the identical full target and a valid commit, equal to the baseline commit when supplied; added digests remain separately audited under 7. Changed targets/hashes remain visible. | Independent; previous full-target metadata fix is verified in this fresh capture. |

### Remaining replay rows and live verification

The final **eight actionable replay rows** are two apple-action version/digest
fields, three satisfying Terraform provider declarations, one azure_core floor,
one TypeScript peer widening field and its obsolete exact range pin. All map to
the implementation fixes above; none is unexplained. The old report is neither
edited nor regenerated from invented upstream data.

Offline regressions cover release/tagger/undated precedence, late release
publication and old cache invalidation; peer ranges/exact peers/installed pins;
Cargo floors/provider constraints; typed Node LTS selection and setup-node dates;
and positive/negative conformance guards. Existing public cassettes have explicitly
labelled synthetic empty release-history additions for the new tag reads; these
are not fresh recordings. Date semantics are independently tested with synthetic
transport responses. Ordinary tests do not use the network.

Validation passed with `CARGO_NET_OFFLINE=true mise run ci`: 22 cassettes scanned,
formatting, workspace/all-target Clippy with `-D warnings`, workspace tests and
the workspace build. Log: `/root/rustovate-conf/r12-round4-ci.log`.

**Phase 1 exit remains pending a live rerun**, rather than claiming that an
unchanged report proves new behavior. This implementation process has none of
`PATCHULA_TOKEN`, `GITHUB_TOKEN` or `GH_TOKEN` configured. Build the committed
release implementation, rerun all twelve repos cold/warm/warm2 with one fresh
cache, then compare against the fresh baseline (refresh the baseline again if
the captures are no longer contemporaneous). Confirm the eight fixed fields,
proposal changes expected from peer-pin removal, and deterministic
proposal sets; remeasure timings because release-date/LTS metadata adds reads.

Coverage remains **eleven comparable repositories and one gap**: remote-agent's
Renovate configuration validation failed. This is not parity for that repo.
All three supplied reports have six warnings: four private-GHCR 401 catalog/digest
reads, one 1password/cli GraphQL failure and one Go proxy 404. No rate-limited
digest differences remain. These lookup/coverage limitations are explicit and
grant no new comparison exemptions.

## Round 3: supplied live batch and final offline replay

The supplied release batch at `1fa827c`, with one fresh SQLite cache and all
twelve repositories in a single process, took **36.9 s cold / 4.2 s warm /
4.1 s warm2**, with **1,750 / 163 / 151 requests**, **0 / 76 / 122 304s** and
**331 / 331 / 334 proposals**. Warm2 contains 27 warnings. These are live
observations from the caller, not measurements of the round 3 implementation.

Final replay of the unchanged warm2 report against the supplied baseline:
**8 missing, 6 extra, 21 field-mismatch, 263 deliberate departures**. Of the
original 57 actionable rows, 22 now have narrow evidence-backed classifications;
35 remain visible. All eight missing/absent registry digest rows now carry
rate-limit annotations (previously only four did). Eleven repositories have a
baseline; remote-agent remains a coverage gap, not parity. Private reports stay
under `/root/rustovate-conf/out12-r3-final/`; none are committed. Replaying old
output cannot demonstrate the extraction/lookup fixes below. Phase 1 exit still
requires the caller's new live run and a fresh baseline.

### All 57 original actionable rows

Row numbers are the original triage JSON's non-departure rows, in order. Grouped
entries cover every row exactly once. “Retained” means no automated exemption.

| Original rows | Finding and disposition |
|---|---|
| 1–3 | Terraform AWS: explicit exact provider pins versus unchanged `~> 6.0`, with identical 6.69.0 targets. Departure 41; runtime/module support constraints do not qualify. |
| 4 | svelte-table: separate 9.0.1 patch track alongside the age-qualified minor. Departure 33, including the independent minor-age evidence. |
| 5–6 | mise Node 26 versus a baseline following 24/LTS. Narrow policy departure 42; retained because reports do not prove channel membership. Choose an explicit permitted line if LTS is intended. |
| 7–8, 11, 14 | mise-action target commits belong to v5.1.2 versus v5.1.1; the newer target is 67 h 39 min old. Departure 23 now covers the paired commit hash with ordered versions, a 24-hour policy and equal current declaration/digest. Same-version changed hashes remain actionable. This is a 24-versus-72-hour policy difference, not one-hour time skew. |
| 9–10, 22–25 | setup-node and @types/node target 26 versus 24. Departure 42 documents the independent stable policy; retained for an explicit channel decision. The Node index and setup-action catalog also have different timestamp precision. |
| 12–13 | apple-actions v7.1.0 versus v7.0.0, with corresponding different commits. Retained: the selected commit is dated Oct 9, outside both age windows. A lightweight tag's creation time cannot be inferred from that date. Refresh baseline and capture tag/release timing before deciding. |
| 15 | azure_core target 1.1.0 with preserved floor 1.1 versus 1.0. Departure 40, scoped to equal-target bare two-component Cargo ranges and explicit preserve policy. |
| 16–19 | keepass 0.15.2 versus 0.15.0 and toml_edit 0.25.17+spec-1.1.0 versus 0.25.16, plus Cargo compatibility taxonomy. Retained: actual releases are older than 72 hours; taxonomy alone cannot excuse different targets. Recompare fresh catalogs. |
| 20–21 | Published mise backends select bitwarden/sdk-sm and npm prettier. Departure 44 records the reason; distinct package identities remain compared. Bitwarden extraction now selects bws-v tags, not language SDK tags. |
| 26 | @types/node's satisfying 22.20.5 target is an exact pin rather than an unchanged-range patch. Departure 25 requires the identical target, satisfying current range and explicit exact policy. |
| 27 | Immich patch fallback while the exact baseline minor is held for 24 hours. Departure 33; expired or unrelated holds cannot qualify. |
| 28–29, 31–32 | setup-go returned an empty matching-refs namespace. Fixed with a fully paginated tags-API fallback and peeled-object hydration; cold/warm regression added. Retained in this old report pending live verification. A genuinely empty catalog still warns. |
| 30 | Explicit setup-bun latest pin, source npm/tool. Departure 29 now recognizes that tool source as well as GitHub releases. |
| 33–34 | TypeScript peer widening versus explicit exact major/pin policy. Departure 25 accepts only the observed `current || target-range` shape and matching peer metadata. Exact peer pins are flagged as questionable; prefer a deliberate support-range policy for published libraries. |
| 35–37 | Go language floor 1.26 versus baseline's resolved compiler 1.27.2; patchula offers explicit floor edits to 1.26.9/1.27.2. Departure 43 documents the model; retained without a generic Go-version exemption. |
| 38–42, 44–46 | Eight missing/absent Docker digests: Hermes, Python slim/alpine, Alpine and Stalwart. Every row has exact 429/rate-limit evidence. The host-wide zero-pull cooldown bug is fixed; retained pending successful live HEADs. No rate-limit exemption. |
| 43, 47–48 | OpenClaw, Victoria Metrics and regex package patch alternatives. Departure 33 consistently ignores npm integrity/Helm archive hashes and requires a matching minor or independently evidenced age-held minor. No package-specific exception. |
| 49–52 | Action digest pins: preserve v4/v2 ref precision (departure 45), but newVersion must be the full target version. Fixed the metadata bug, including resolving the new commit on an existing digest update. All four old-report fields remain visible for rerun. |
| 53–57 | Exact npm major edits versus lower caret floors. Departure 25 requires a containing target-major range; independent held-target/clock evidence is also required where release-age policy changes the selected version. |

### Proposal determinism

The warm runs started at 05:51:22 and 05:51:26 UTC, seconds apart. Their only
proposal-set changes are the two vaultwarden/server Compose pins and the
hashicorp/vault Compose pin. Cold/warm logged 429 failures; warm2 successfully
sent HEADs and returned the digests. No release boundary caused these three
changes. A successful Docker Hub response with `ratelimit-remaining: 0` had set
a host-wide cooldown, so queued non-consuming HEADs could fail depending on
response completion order. Pull cooldown now gates only Docker Hub manifest
GETs. Actual HEAD/tag-list throttling still establishes a general cooldown;
stale provenance remains explicit. See [Docker's pull accounting](https://docs.docker.com/docker-hub/usage/pulls/).

Offline regressions exercise both request orders, cold/warm HTTP entries and
client reopen; other HEADs succeed while manifest GETs are blocked by zero pull
quota. An additional fixed-time permutation test covers equal-precedence build
metadata, exact/range/latest selectors and the one-second age boundary. Candidate
ties now use the complete source record rather than input order. Existing engine
replay compares full cold/warm repository reports at one fixed evaluation time.
Different upstream failures or changed responses are different inputs; genuine
network outages can still change proposal availability and remain warnings.

### Time skew, separately

The baseline was captured **04:44:02–04:51:55 UTC**; patchula evaluated at
**05:51:26 UTC**. Five comparable occurrences crossed the 24-hour eligibility
boundary between them: wrangler 4.151.0 in clips.sweepy.dev's mise.toml and
package.json and maccrae-wiki's package.json (eligible 05:50:43 UTC), and uv
0.13.1 in devops-minter's .mise.toml and maccrae-infra's mise.toml (eligible
05:15:30 UTC). They are already departure-23 rows, not members of the 57.
remote-agent's nightly t3code also crossed during the baseline window but has
no baseline. **None of the 57 is established one-hour time skew.** No rule was
added for capture skew. In particular, the Cargo/apple-action target differences
are retained rather than attributed to timing without evidence.

### Review of all 27 warm2 warnings

- **16 quota failures:** infisical, postgres catalog/current/target, redis
  catalog/target, Alpine catalog/target, notifuse, Hermes, OpenCloud, Python
  catalog/two targets and Stalwart catalog/target. Successful zero-counter HEADs
  no longer suppress queued reads. Real 429s remain explicit lookup failures;
  rerun to distinguish remaining upstream limits from the fixed local gate.
- **2 stale successes:** OpenClaw and system-upgrade-controller digests. Expected
  fallback with original fetch timestamps, not renewed freshness; departure 36
  has no parity exemption.
- **4 unauthorized reads:** catalog and digest for each of two private GHCR
  packages. Expected anonymous 401s. The warning identifies package/status;
  provide registry-scoped access or accept unavailable lookup. No new auth scope.
- **3 fixed extraction/availability diagnoses:** jq's jq- tags and Bitwarden's
  bws-v tags now parse; setup-go's empty namespace gets the tags fallback. Tests
  cover these paths. An actually empty catalog remains clearly warned.
- **2 unresolved upstream-source conditions:** 1password/cli GraphQL unavailable
  and the Go proxy's clbanning/mxj/v2 404. These are expected failures for the
  selected sources, not dependency absence. Check the vendor's supported public
  version endpoint and the module's proxy availability separately; do not hide
  either warning or read/execute a mise plugin to work around it.

The final implementation is verified with `CARGO_NET_OFFLINE=true mise run ci`
(edition 2024, forbid unsafe, formatting, Clippy `-D warnings`, fixture scan,
offline tests and workspace build). No upstream implementation/presets/fixtures,
npm command, standalone script or repository-specific product rule was added.
All commits remain local. The next live report must confirm the HEAD, setup-go,
tag-prefix and action metadata fixes before Phase 1 exit is declared.
Implementation commit: `af88256`; CI log: `/root/rustovate-conf/r12-round3-ci.log`.

## Historical round 2

Round 2 replay of the supplied twelve-repository warm capture (2026-10-15):
**22 patchula-missing, 12 extra, 60 field-mismatch, 207 departures**. Eleven
repositories have a comparable baseline; remote-agent still lacks one because
the baseline rejected its configuration. These counts describe the supplied
capture with the updated adapter, not a new live engine run.

Compared with 55/12/61/174, 33 missing Cargo proposals are now classified as
unchanged-manifest no-ops (departure 35), and one Cargo Git URL in mise is
canonicalized to the same GitHub repository identity. Eight remaining rows
have exact rate-limit evidence: four absent digest/pin proposals and four absent
target digests. They remain included in the actionable counts and carry
`rateLimited: true` in triage JSON. This annotation does not classify disabled
version upgrades as rate-limit failures.

The new engine follows bounded HTTPS redirects (including public Helm indexes),
reuses rate-limited cached catalogs/digests with original fetch timestamps,
honors exhausted Docker Hub quota windows, and accepts optional Hub credentials.
It also extracts explicit setup-bun versions. Extraction fingerprints changed
to invalidate old cached action input extraction. New report evidence records
effective disabled classes, deprecated/yanked target rejection, batch evaluation
time, and lockfile/pinned-commit resolution provenance. Departures 37–39 and the
missing-target part of departure 23 require that evidence, so the supplied old
report cannot use them. Registry availability behavior is departure 36 without
a comparison exemption.

Residual causes and next checks are scoped as follows:

- **Registry digests (8 rows):** Docker Hub quota exhaustion only. Rerun warm
  with optional Hub credentials; verify stale metadata and warnings for cache
  fallbacks. A missing cache entry still fails explicitly.
- **Kubernetes majors (2 missing):** explicit per-file PostgreSQL major holds,
  including Immich's PostgreSQL 14 constraint. These are policy differences,
  independent of quota. New reports expose the disabled major class.
- **Flux (3 missing, 1 extra after rate-related rows):** two apisix patch targets
  are inside the 24-hour hold, external-secrets failed on a 302, and Victoria
  Metrics has a patch alternative. HTTPS redirects are fixed; rerun for age and
  candidate evidence. Do not exempt arbitrary chart changes.
- **GitHub Actions (5 missing, 28 mismatches):** setup-bun extraction accounts
  for one missing dependency. setup-go returned an empty tag namespace and its
  commit lookup returned 422: four missing proposals and two current-version
  differences require a healthy upstream response. Twelve other current-version
  differences compare a pinned commit with the current floating tag. The new
  provenance makes that distinction reviewable. Five differing target digests,
  action pin/ref precision and Node current-versus-LTS targets remain visible;
  compare contemporaneous tag and commit catalogs before exempting any hashes.
- **npm (5 missing, 3 extra, 21 mismatches):** the cached public catalogs mark
  mux-node 15.5.0 and plyr 3.8.5 deprecated; convex-svelte and better-auth targets
  are held by age. An @types/node satisfying-range target is represented by
  patchula's exact pin rather than a patch, while its major target follows a
  different Node channel. Thirteen current-version differences are lockfile
  versus satisfying-range resolution. Remaining value differences include
  peer unions, short caret ranges and differing major range floors. Extra
  prerelease-to-stable/patch alternatives require candidate evidence; none gets
  a general source/manager exemption.
- **Cargo (2 missing, 2 extra, 1 mismatch):** keepass and toml_edit differ in
  both target and pre-1.0 taxonomy; azure_core has a different preserved range
  floor. These are not manifest no-ops. Capture paired catalogs and decide
  whether the desired range edit retains a compatibility floor or tracks the
  selected release's precision. Current rules keep these actionable.
- **mise (2 extras, 2 package mismatches):** Node current versus LTS explains
  the extras. The published registry chooses different backend identities for
  bitwarden-secrets-manager and prettier. Prefer explicit backend/channel
  configuration if parity is wanted; do not equate different package identities.
- **Go (2 extras, 1 mismatch):** the minimum language declaration is distinct
  from the installed toolchain. A future toolchain edit must retain that
  distinction; these rows have no generic current-version exemption.
- **Terraform (3 value mismatches):** the baseline retains the AWS provider's
  `~> 6.0` support constraint while explicit exact policy selects a concrete
  version. Choose preserve policy if the constraint is intentional; do not
  automatically equate interval support with an installation pin.
- **Regex (1 missing, 2 extras):** Renovate's minor target is inside the 24-hour
  hold; its fallback patch and the immich patch alternative need paired evidence.
- **Other availability:** registry.k8s.io's 307 and the Helm 302 now follow
  the safe redirect path. Private GHCR 401s, the Go proxy module 404, and the
  1password GitHub GraphQL failure need source access or identity diagnosis.
  They do not justify widening public auth scopes or hiding dependencies.

For the next comparison, rerun the release batch using the existing cache and
example configurations, then compare its report with a contemporaneous baseline.
Do not subtract predicted fixes from these replay counts. Phase 1 exit remains
pending that live rerun and resolution of the residual catalog/policy choices.

The comparison uses observable lookup output, repository-owned configuration,
and small hand-written offline fixtures. Raw repository files, logs, reports,
registry responses and generated triage stay outside this repository. No
upstream implementation, presets or fixtures were used.

The supplied warm report predates these implementation changes. Replaying it
validates the adapter and its classifications; it cannot demonstrate the new
engine's live output or certify Phase 1 exit. Refresh the batch with the example
configs, then compare that report with a contemporaneous baseline. Extraction
fingerprints were bumped so a warm cache will not resurrect the old extraction.

| Bucket | Cause and implementation | Offline regression |
|---|---|---|
| Bun/npm | Both describe npm dependencies in the same package.json. Canonicalize only that manifest and npm/unspecified source, preserving identity and fields. | `bun_alias_is_manifest_and_source_scoped_and_preserves_changes` |
| Kubernetes/Helm-values | API inventory and duplicate image ownership inflated missing rows. Audit narrowly scoped exclusions; retain changed proposals and missing owners. ConfigMap image environment variables also needed concrete extraction. | `api_inventory_and_identical_yaml_duplicates_are_audited_but_proposals_survive`; `generated_flux_bundle_and_configmap_image_variables_are_concrete_dependencies` |
| Flux | Join observable registry URL/chart identity, separate archive checksums from OCI digests, and recognize generated Flux bundle versions. Bundle updates will require regeneration in the future writer. | `helm_registry_and_checksum_adapter_does_not_hide_registry_changes`; generated-bundle regression above |
| Docker images | Floating tags were compared with zero-filled versions, allowing patch-precision and date tags onto another tag track. Require matching numeric precision and compatibility suffixes. Numeric report versions normalize separately from literal tags. | `docker_alias_precision_prevents_patch_and_date_tag_proposals` |
| GitHub Actions | Resolve partial refs against the pinned commit, preserve ref precision, emit same-alias changes only as digests, and pin proposed upgrades to the selected full tag's commit. Extract explicit setup-tool versions with distinct editable locations. | `floating_action_aliases_resolve_by_commit_and_keep_comment_precision`; `setup_inputs_have_their_own_editable_locations` |
| Requirements | Exact equality pins lost their known resolved version. Preserve valid exact pins without inventing versions for ranges or wildcards. | `exact_requirements_and_go_pseudo_versions_retain_observed_identity` |
| Go | Preserve pseudo-version commit identities. Keep intentional indirect upgrades separately classified. A new major requiring a different module path is not a valid version-only proposal. | pseudo-version regression above; `indirect_go_and_test_compose_departures_do_not_extend_to_direct_or_production`; `go_major_path_rule_preserves_incompatible_releases_and_other_sources` |
| Cargo | Satisfying releases generated manifest no-ops; pre-1.0 compatibility classification collapsed the next compatibility boundary and numeric-major alternative into one proposal. Suppress no-ops and retain both tracks. | `cargo_noop_ranges_are_not_updates_and_both_breaking_tracks_survive` |
| Terraform | OpenTofu lockfile registry identities were not recognized. Extract runtime support constraints as well as providers, and preserve support intervals by default. | `terraform_opentofu_locks_and_runtime_constraints_use_the_same_identity`; `runtime_support_intervals_are_observed_without_spurious_exact_pins` |
| mise | Resolve numeric selectors as release lines and Cargo Git selectors through GitHub tags, retaining their source identity. Latest pins and version prefixes have separate adapter handling. | `numeric_mise_selectors_resolve_the_release_line_without_fake_zero_patches`; `mise_git_tags_use_github_without_the_selector_prefix`; latest-pin adapter regression |
| npm | Classify an exact proposal satisfying an existing range as one pin, even with an older lock. Extract Node engine constraints without replacing supported runtime intervals with installation pins. Correct the example that enabled an inactive legacy custom manager. | `satisfying_npm_range_upgrade_is_one_pin_even_with_an_older_lock`; runtime-interval regression above |
| Missing baseline | Invalid configuration and absent snapshots are coverage failures, not a collection of extra dependencies. A successful empty baseline remains comparable. | `invalid_or_absent_baseline_is_coverage_and_empty_success_is_comparable` |

The full comparison rules, including negative conditions, are in
[departures](/renovate/departures) and the conformance regression tests. Exclusions,
normalizations, departures and no-baseline coverage remain separately visible.
