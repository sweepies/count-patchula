# Phase 1 wave 2 coverage

All 13 Pkl examples validate (12 comparison repositories and benchmark-public).
The offline engine coverage test applies every config to independently authored
representative manifests and evaluates extracted source/versioning combinations.
It asserts Cargo/Compose for fnox, Go for immich-go, Terraform for
cafeteria-order-tracker, and Kubernetes/Kustomize/Flux/Helm values/requirements for
maccrae-infra. Existing npm/Bun, lockfile, workflow and mise tests remain active.
Manager, source and scheme dispatch cover every value in the bundled schema;
lookup has no remaining "not implemented" branch for these enums.

This is a config/dispatch and fixture check, not a fresh inventory of the twelve
repositories. Their live comparison remains the next integration check. No
Renovate source, presets or fixtures were inspected. New fixture bodies came
from public registry endpoints and public jdx repositories via the recording
tasks; ordinary tests replay without constructing a network transport.

Release-mode offline replay on this workspace, with compilation outside measured
intervals:

- Wave 2 registry sources: 24.5–36.4 ms cold, 7.6–14.0 ms warm across two runs.
  Cold: 13 HTTP requests. Warm: 5 requests, including 3 conditional 304s.
  The second run used 370,616 bytes for SQLite, WAL and shared-memory files.
- Existing public source fixture: 36.8 ms cold, 14.3 ms warm; 19 cold requests,
  3 warm requests and 2 warm 304s.
- Public two-repository engine fixture, now including Cargo: 337 ms cold,
  82.9 ms warm; 58 cold requests, 6 warm requests, 1 warm 304, four unique
  catalog lookups and two warm extraction hits.

These are small fixture replays, not estimates of live network latency. The
provided live renovate-runner baseline (4.0 s cold, 0.9–1.1 s warm, 0.85 MB cache)
has not been remeasured here. New shared-index/registry reads coalesce overlapping
requests; later sequential calls retain normal freshness/revalidation behavior.
Token refresh is single-flight per repository, while manifests can fetch in
parallel. Registry tags use complete lexical pagination because a known tag is
not a safe stopping point. Catalogs and digest keys remain shared across a batch.

## Remaining limits and recommendations

- Registry authentication covers anonymous public Docker Hub/GHCR bearer flows.
  Private registry credentials, arbitrary cross-origin token services, registry
  mirrors, Terraform custom hosts and provider service discovery are not wired.
  Keep those explicit extensions; do not send GitHub credentials to registries.
- Cargo git/custom registries, Terraform git/local modules, Kustomize remote git
  resources and pip editable/VCS/URL dependencies are not registry update targets.
  Cargo/Terraform locks enrich direct declarations; transitive lock entries do
  not become independent dependencies.
- YAML image aliases/templates and rendered Helm templates need concrete scalar
  locations. Helm images support strings and repository/tag/digest maps, not all
  chart-specific naming conventions. Flux source references must be present in
  selected files; chartRef releases use the OCIRepository version separately.
  Digest-only OCI refs cannot infer a release tag without exhaustive manifests.
- Requirements extraction handles versioned requirement lines with extras,
  markers and hashes. Unpinned requirements, arbitrary include/constraint file
  traversal and custom pip indexes need a separate policy/configuration design.
- Go replace/exclude/retract semantics and Docker ARG default/conditional shell
  operators are not interpreted. Docker supports ordinary global ARG substitution
  and stage aliases. Go/toolchain directives have distinct kinds but share the
  public Go release backend.
- Preserve/widen retains satisfied constraints; unsupported compound range edits
  fail explicitly. Retain the default exact policy for the comparison run.
- Docker publication timestamps are unavailable in the standard tag list, so
  those releases have no age delay. Rate limits surface Retry-After; automatic
  retry storms are avoided. Decide on a larger retry budget only after observing
  the twelve-repo live run.
- Mutation, dependency installation, lockfile rewriting and the CLI run command
  remain later-phase work. Lookup is read-only.

There are no blocking configuration questions for the requested lookup matrix.
Recommended next check: run the twelve-repository live comparison, inspect
warnings for these limits, and compare cold/warm request counts and cache size.
