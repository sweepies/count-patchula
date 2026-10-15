# Phase 1 comparison follow-up

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
| 35–37 | Go language floor 1.26 versus baseline's resolved compiler 1.27.2; rustovate offers explicit floor edits to 1.26.9/1.27.2. Departure 43 documents the model; retained without a generic Go-version exemption. |
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

The baseline was captured **04:44:02–04:51:55 UTC**; rustovate evaluated at
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
**22 rustovate-missing, 12 extra, 60 field-mismatch, 207 departures**. Eleven
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
  rustovate's exact pin rather than a patch, while its major target follows a
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
[departures](departures.md) and the conformance regression tests. Exclusions,
normalizations, departures and no-baseline coverage remain separately visible.
