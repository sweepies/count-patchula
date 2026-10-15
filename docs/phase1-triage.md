# Phase 1 comparison follow-up

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

The remaining evidence and policy work is deliberately kept actionable:

1. **Fresh output after the fixes.** Action alias rows, exact requirements,
   OpenTofu lock versions, setup inputs, generated bundles, ConfigMap image
   values, Cargo no-ops/tracks, npm pin taxonomy and the corrected example still
   occur in the old report. Rerun before attributing those counts to new code.
2. **Current version semantics.** The baseline can report the newest satisfying
   npm release while rustovate reports the locked release; minimum Go language
   declarations are another distinct concept. Add resolution provenance and
   compare declared, locked and resolved fields separately before exempting them.
3. **Policy evidence.** The baseline snapshot contains major image candidates
   disabled by explicit repository rules. Keep those rules. Expose disabled
   update classes in reports before classifying missing proposals. Provider
   constraints, peer unions and Cargo range floors also need an explicit choice
   between supported intervals and installation pins; do not silently rewrite
   the configuration to obtain a smaller difference count.
4. **Release-age-only differences.** Held-back metadata explains some missing
   proposals and fallback patches, but the current rule requires an actual
   paired target with timestamp evidence. Add a batch evaluation timestamp and
   rejected-candidate reasons before expanding it to missing/extra proposals.
5. **Deprecated release rejection.** Public registry checks confirmed that two
   missing npm targets are deprecated. The evaluator intentionally rejects
   deprecated versions, but the report does not preserve that evidence. Add
   rejection metadata and a narrowly evidenced departure rather than hiding
   arbitrary missing targets.
6. **Backend and runtime channels.** The checked-in mise registry selects
   different valid backend identities from the baseline for some shorthands.
   Node current/LTS selection also differs. Use explicit backend/channel policy
   if parity is intended; otherwise document and prove a scoped departure.
7. **Upstream availability.** The captured batch includes Helm redirects and
   registry authorization errors. Diagnose authenticated/redirect-capable access
   separately; a completed repository report can still contain lookup warnings.
8. **Residual release/catalog targets.** A few Cargo targets and fallback tracks
   have different resolved releases without sufficient paired-age evidence.
   Capture contemporaneous public catalogs and replay them offline after the
   live rerun. Keep these differences visible until that evidence establishes
   whether the cause is filtering, timing or a versioning bug.

No per-host governor, registry token/tag caching, or Go proxy freshness changes
are part of this work.
