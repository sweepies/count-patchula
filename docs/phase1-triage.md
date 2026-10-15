# Phase 1 comparison follow-up

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
