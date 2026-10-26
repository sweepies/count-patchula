---
title: "Deliberate departures"
---
The Count is not a Renovate clone. Where he behaves differently on purpose, the
difference is written down here with a reason.

Some entries also explain differences in the conformance comparison, which runs
Renovate and the Count on the same repositories. Their **Comparison harness**
block says exactly which differences the entry explains. Any difference outside
those rules is a real one, and an entry without a block explains none.

<details><summary>Comparison harness: normalizations for every entry</summary>

These are counted, and never hide a value, digest or target difference:

- Bun's npm-sourced (or source-less) `package.json` rows compare as `npm`.
- Numeric `v` prefixes on mise versions, regex GitHub versions and Flux chart
  versions are dropped. The literal current and proposed values are kept.
- Docker numeric `version` fields are canonicalized; `value` keeps the full
  variant tag.
- An absent baseline `currentVersion`, or an absent `newVersion` on an otherwise
  identical digest proposal, is unknown rather than a difference.
- A missing or invalid baseline is reported on its own and never implies parity.
</details>

## Configuration

### Pkl configuration {#pkl-configuration}

- **Renovate:** JSON/JSON5 with presets, migrations
- **Count Patchula:** Pkl only, typed schema, unknown fields are errors
- **Why:** Typed, composable config with real diagnostics

### Explicit defaults {#explicit-defaults}

- **Renovate:** Repository configs may select opaque preset names
- **Count Patchula:** Explicit schema defaults: exact versions, action/image digests, manual merges, separate PRs, release notes, dependency label and semantic prefix
- **Why:** Auditable independent policy; no preset expansion or AGPL data

<details><summary>Comparison harness</summary>

Digest additions for actions and images are explained here. The extra pin must
keep the current value and version, add a digest to an unpinned dependency, and
have no group.
</details>

### Default excluded paths {#default-exclude}

- **Renovate:** `ignorePaths` defaults to `node_modules` and `bower_components`; `vendor`, test and example directories are ignored only by the `config:recommended` preset, and `fixtures` by neither. A repository's `ignorePaths` replaces the list
- **Count Patchula:** `exclude` defaults to `node_modules`, `bower_components`, `vendor`, `.git`, `test`, `tests`, `__tests__`, `__fixtures__`, `fixtures` and `examples` at any depth. Amending adds to the defaults and a `!` entry re-includes a path; assigning a new Listing replaces them
- **Why:** With no preset layer, the defaults carry what a recommended setup ignores; test data and examples hold manifests nobody deploys, so updating them is noise

### Limits {#limits}

- **Renovate:** Zero can represent unlimited limits
- **Count Patchula:** Null means unlimited by default; positive integers limit PRs; zero is rejected
- **Why:** Avoid overloading a numeric limit

### Prefixed patterns {#match-syntax}

- **Renovate:** Match fields accept either a slash-delimited regex or a glob
- **Count Patchula:** Explicit `regex:` or `glob:` prefixes, optional `!`; path-aware globs by default
- **Why:** Avoid delimiter guessing; preserve literal slashes

### Rules and selectors {#rules}

- **Renovate:** `packageRules` with many `match*` keys and settable options
- **Count Patchula:** Typed selectors and policy overrides; ordered replacement, including whole lists; empty group means separate PRs
- **Why:** Smaller, explicit policy model without compatibility aliases

### One extractor model {#custom-extractors}

- **Renovate:** Regex custom managers with `currentValue`/`depName` captures and template strings
- **Count Patchula:** One extractor model; current/source/dependency captures and simple `{capture}` substitution; Rust regex syntax
- **Why:** One validated model without executable template helpers or lookaround

### Sharing configuration {#shared-configuration}

- **Renovate:** Named presets and `local>` references
- **Count Patchula:** Ordinary local Pkl amends/import; the bundled schema can be vendored; no remote imports or external resource reads
- **Why:** Deterministic validation with no network or environment dependency

### Release age and windows {#release-timing}

- **Renovate:** Duration strings, schedules and timezone keys
- **Count Patchula:** Nonnegative waitHours plus explicit cron/timezone windows, each time a window names being due on the first run after it; empty windows means unrestricted
- **Why:** Typed units and validated timezones; no natural-language scheduling

### Manager names {#manager-names}

- **Renovate:** Ecosystem names such as `gomod`, `pip_requirements` and `custom.regex`, with `managerFilePatterns`
- **Count Patchula:** Closed manager identifiers (go, requirements, regex, compose); explicit per-manager file sets; no legacy aliases
- **Why:** Fail on typos and keep the independent vocabulary small

### Branch and commit names {#branch-names}

- **Renovate:** `renovate/` branch prefix; `semanticCommits` switches commit message style
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
- **Count Patchula:** No equivalent manager
- **Why:** The Count uses Pkl and does not resolve Renovate configuration or presets

<details><summary>Comparison harness</summary>

The fixed rule `renovate-config-manager` removes Renovate's own `renovate-config`
manager, including its branch upgrade summaries, before normalization. Triage
lists the repository, manager and counts under `exclusions`. No config contents
or preset data are read. Every other manager, including unknown names, is still
compared. The rule is independent of `conformance/departures.json`.
</details>

## How the Count runs

### No onboarding pull requests {#no-onboarding}

- **Renovate:** Opens an onboarding PR in repos without config
- **Count Patchula:** Repos without config are skipped. In a local working tree with no config, the CLI offers its defaults or a starter `patchula.pkl` at a terminal, and `--default-config` uses the defaults without asking
- **Why:** Configs are added deliberately; a person running the CLI in their own checkout is the one deciding

### Token from the GitHub CLI {#gh-token}

- **Renovate:** The token comes from its own configuration or environment
- **Count Patchula:** `PATCHULA_TOKEN`, `GITHUB_TOKEN`, `GH_TOKEN`, then a signed-in GitHub CLI's github.com token
- **Why:** A local `lookup` or dry run works on a machine already signed in to `gh`, with no token to copy

### One process per batch {#batch-execution}

- **Renovate:** One repository at a time, caches reset per repository
- **Count Patchula:** One process per batch, concurrent repos, batch-wide lookups
- **Why:** Speed

### Persistent user cache directory {#cache-directory}

- **Renovate:** `cacheDir` defaults to a directory under the OS temporary directory
- **Count Patchula:** Local commands default to `patchula/` in the user cache directory (`XDG_CACHE_HOME` or `~/.cache`, `~/Library/Caches` on macOS), shared by every checkout; `--cache DIR` overrides it
- **Why:** Lookups made for one checkout are reused in the next, the cache survives reboots that clear temporary files, and nothing is written into the repository

### Incremental release history {#incremental-release-history}

- **Renovate:** Fetches full release and tag lists
- **Count Patchula:** Incremental, newest-first, stops at the first known item
- **Why:** Speed and rate limits

### Reading through the API {#no-clones}

- **Renovate:** Clones each repository
- **Count Patchula:** Reads files through the API; clones only for lockfile tools
- **Why:** Speed

### Writing the working tree {#local-apply}

- **Renovate:** The local platform only does dry runs; it writes no files
- **Count Patchula:** `patchula apply` and the terminal UI write updates into the working tree, with no branches or PRs
- **Why:** Pinning and updating a checkout from scripts and pre-commit hooks, with the change left for you to review and commit

### A JSON report {#json-report}

- **Renovate:** Log lines
- **Count Patchula:** Versioned JSON report
- **Why:** Logs are not an API

### Conditional source refresh {#source-refresh}

- **Renovate:** Histories are exposed through registry/platform responses
- **Count Patchula:** A conditional REST probe of the history head gates GraphQL; registry bodies are reused while their declared freshness lasts, and Go proxy lists and validator-free OCI tag pages get a one-hour floor
- **Why:** Avoid refetching unchanged histories; GraphQL POSTs have no conditional HTTP validators

The floor can delay a new version by up to an hour; see
[fetching less](/guide/sources/#fetching-less).

### Rate-limited registries {#registry-availability}

- **Renovate:** Lookup may fail on quota exhaustion or HTTPS redirects
- **Count Patchula:** Follow bounded HTTPS redirects; reuse rate-limited cached data with stale provenance
- **Why:** Preserve useful results while exposing freshness limits

A stale copy is listed under `metadata.stale` with a warning; a cold miss still
fails. See [registries and limits](/guide/sources/#registries-and-limits).

### mise-versions catalogs {#mise-versions-catalogs}

- **Renovate:** Upstream API catalogs
- **Count Patchula:** Public mise-versions release/tag lists for plain GitHub lookups, conditional by ETag; mise tools are listed by mise, which reads the mirror itself; `useVersionsHost = false` restores direct sources for both
- **Why:** Fewer GitHub API reads

See [mise-versions host](/guide/sources/#mise-versions-host) for when the
Count falls back to GitHub.

### Release-age catalog reuse {#catalog-reuse}

- **Renovate:** Revalidate on every run
- **Count Patchula:** Reuse a successful, complete catalog for `clamp(waitHours - 1, 1, 24)` hours; `waitHours = 0` keeps normal freshness, and a [recheck](/guide/dashboard/) skips reuse
- **Why:** A release that must wait a day does not need a fresh catalog every run

Eligibility is still decided at run time, and the report shows when each
catalog was fetched. Mutable digests keep their own freshness.

## What gets extracted

### Repeated dependencies {#repeated-dependencies}

- **Renovate:** Manager snapshots can contain repeated dependency names
- **Count Patchula:** Optional occurrence IDs preserve different current values, merging identical rows
- **Why:** Retain all manifest occurrences without ambiguous report identities

Repeated manager/name pairs with different package or current fields, such as
several refs to one action or several mise versions, get occurrence IDs.
Identical occurrences merge their proposals and locations.

<details><summary>Comparison harness</summary>

The adapter applies the same normalization to Renovate's output, so nothing is
excluded. Unlabelled duplicate identities are invalid.
</details>

### Kubernetes API versions {#kubernetes-api-versions}

- **Renovate:** Reports apiVersion/kind pairs as kubernetes-api dependencies
- **Count Patchula:** Kubernetes extraction owns workload images; API migrations require a separate schema-aware operation
- **Why:** Changing CRD API versions without migrating their fields is unsafe

<details><summary>Comparison harness</summary>

Excludes `kubernetes` rows with datasource `kubernetes-api` and no proposals,
and counts them in exclusions. API migration proposals are still compared.
</details>

### One owner per YAML image {#yaml-image-ownership}

- **Renovate:** Broad helm-values file patterns can repeat Kubernetes images
- **Count Patchula:** Kubernetes workload images belong to kubernetes; HelmRelease values belong to flux
- **Why:** Apply one manager policy to each semantic image location

<details><summary>Comparison harness</summary>

Excludes a `helm-values` Docker row only when Renovate has the same package,
current fields and proposals under `kubernetes` or `flux` in the same file, and
the Count reports that owner and package there. A `kubernetes` image already
owned by `flux` in a HelmRelease is handled the same way. Any difference in
version, digest, proposals, file or owner keeps the row, as do standalone
values. Every removed row and proposal is counted.
</details>

### Indirect Go requirements {#indirect-go-requirements}

- **Renovate:** The compared output skips indirect requirements
- **Count Patchula:** Explicit go.mod requirements are maintained, including indirect ones
- **Why:** Security fixes should not depend on a direct dependency changing

<details><summary>Comparison harness</summary>

Explains only extra version upgrades on a Go dependency Renovate marks
`indirect`, with equal current values, no Renovate proposal, a strictly newer
SemVer target, identical newValue/newVersion and no digest. Direct requirements,
pseudo-version changes and extraction differences are not explained.
</details>

### Compose test services {#compose-test-services}

- **Renovate:** The compared output omits test/docker-compose.* manifests
- **Count Patchula:** Extract and update explicitly declared test service images
- **Why:** Integration test infrastructure also needs maintenance

<details><summary>Comparison harness</summary>

Explains only extra `compose` dependencies in `test/**/docker-compose.*.yml` or
`.yaml`, with the Docker datasource and container-image metadata.
</details>

### Unversioned references {#unversioned-references}

- **Renovate:** Retains unused/unversioned provider, local module and setup-input records
- **Count Patchula:** Reports concrete dependency occurrences that can be looked up or edited
- **Why:** Avoid inventing version locations for references without versions

<details><summary>Comparison harness</summary>

Excludes rows the Count lacks in terraform, github-actions, flux or npm that
have no proposals, no resolved version or digest, and either no current value,
a local `file:`, `./` or `../` reference, or a GitHub expression. Concrete
versions, opaque selectors such as `latest`, every proposal and versioned
setup-action inputs are still compared.
</details>

### Forgejo action refs {#forgejo-action-refs}

- **Renovate:** Its `github-actions` docs list `github-tags` for repository
  refs and `gitea-tags` for URL refs, and say nothing of bare refs in Forgejo
  workflows
- **Count Patchula:** A bare `owner/repo` in `.forgejo/` is looked up on
  `data.forgejo.org`; in `.gitea/` and `.github/` it is looked up on GitHub.
  Runner labels in `.forgejo/` and `.gitea/` workflows are not extracted
- **Why:** Forgejo's and Gitea's docs name those as the default hosts for bare
  refs, so the pinned commit is the one the runner fetches. Their runner labels
  name self-hosted runners, not GitHub's images

An instance's `DEFAULT_ACTIONS_URL` can change the host and is not visible from
the repository; writing the action as a URL removes the guess. Only tags are
pinned on forges. Renovate also matches `workflow-templates/` and
`.github/actions/`; the Count reads root workflow directories and `action.yml`
files only.

### Installed versions {#current-versions}

- **Renovate:** currentVersion can mean newest satisfying release or floating tag
- **Count Patchula:** Current version follows the lockfile or pinned commit when available
- **Why:** Keep installed/pinned identity separate from candidate resolution

<details><summary>Comparison harness</summary>

Explains only `current.version` mismatches on matching declarations, packages
and digests, where the report's resolution metadata equals its
`current.version`. For npm lockfiles, both versions must satisfy the same
declaration and Renovate's must be newer. For pinned GitHub actions, the pin
must be a 40-digit SHA with a numeric partial alias, and both full versions must
be on that alias's line with Renovate's newer. Reports without resolution
metadata cannot use this entry.
</details>

### Go language floor {#go-language-floor}

- **Renovate:** Can resolve the go directive as the installed toolchain
- **Count Patchula:** Report the declared minimum language version and its own upgrades
- **Why:** A go.mod `go` directive declares a language floor, not an installed compiler

<details><summary>Comparison harness</summary>

Requires the exact go/go identity, Renovate's golang-version/golang metadata,
the Count's mise/go-directive metadata, identical manifest values and digests,
and a declared resolution equal to the floor. Only a newer Renovate compiler,
and digest-free exact patch or minor floor edits up to that compiler when
Renovate has no proposals, are explained. Toolchains, modules and targets
beyond the observed compiler are not.
</details>

### mise backends {#mise-backends}

- **Renovate:** Can select older or different backend identities
- **Count Patchula:** Ask the pinned mise to list each tool (`mise ls-remote --json --prerelease`), in a sandbox, and report the tool's own name as its package with the source `mise`
- **Why:** Every backend mise lists works, with mise's own registry and version rules, without the Count keeping a copy of them

So the report differs from Renovate's for every mise tool: the package is
`prettier`, `bitwarden-secrets-manager` or `jq` rather than the backend's
repository or registry package, and the source is `mise`.

Branches are named after the tool too: `patchula/jq-1.x`, not
`patchula/jqlang-jq-1.x`. Tool options are not passed to mise, because some
redirect a backend to another host (`api_url`, for one) and the lookup carries
the GitHub token. See [mise lookups](/guide/sources/#mise-lookups) for the
sandbox, Git-ref tools and release notes.

<details><summary>Comparison harness</summary>

Explains the `package` field only: a tool or toolchain listed by mise whose
package is its own name, with the same current value and digest as Renovate's.
Proposals and targets are compared separately. This is not package equivalence.
</details>

### mise plugins are never run {#mise-plugins}

- **Renovate:** No equivalence is claimed
- **Count Patchula:** Refuse `asdf:` and `vfox:` tools, and registry names that only have a plugin backend, with a lookup warning
- **Why:** An asdf plugin is shell scripts and a vfox plugin is Lua, both cloned from a repository the dependency file names. Running them would run code from the repository the Count is reading, on the machine that holds his token

mise runs with `MISE_DISABLE_BACKENDS=asdf,vfox`, so a registry name such as
`aapt2`, whose only backend is a vfox plugin, fails, and a name that lists a
plugin first and another backend later uses the other backend. See
[mise lookups](/guide/sources/#mise-lookups).

### GitLab CI variables {#gitlab-ci-variables}

- **Renovate:** Resolves variables such as `$CI_REGISTRY` in GitLab CI images through `registryAliases`
- **Count Patchula:** Skips images and include refs built from GitLab CI variables, listing each one with its reason; only `$CI_SERVER_FQDN` and `$CI_SERVER_HOST` at the start of a component reference resolve, to `gitlab.host`
- **Why:** The Count has no registry aliases, and a guessed registry would pin the wrong image

### The GitLab instance {#gitlab-instance}

- **Renovate:** Looks up `include: project:` refs on the GitLab instance it runs against
- **Count Patchula:** Looks them up on `gitlab.host`, gitlab.com by default, which must be gitlab.com or have a `gitlab` label in its name
- **Why:** The Count runs on GitHub, so there is no GitLab platform to ask; recognising a GitLab instance by its name keeps lookups unauthenticated without probing unknown hosts

### Nix flake inputs {#nix-flake-inputs}

- **Renovate:** The `nix` manager is beta and off by default; it matches `flake.nix` and looks inputs up with `git-refs`
- **Count Patchula:** `nix` is on by default; it reads `flake.lock` beside a `flake.nix`, and asks nix itself what each input's reference points at, with `nix flake prefetch`
- **Why:** nix resolves every input type a lock can hold (tarballs, channels and the flake registry included) the way `nix flake update` will, and gives the commit date for the release-age wait. Locking runs no repository code, so there is no reason to keep it opt-in

Inputs that resolve outside the repository, ssh URLs and plain http are
skipped, and branches are named after the input: `patchula/nix-nixpkgs`. See
[Nix](/guide/managers/#nix).

## Which updates are proposed

### Copier templates are reported, not regenerated {#copier-templates}

- **Renovate:** Runs `copier update` to move a template to its new tag, with `--trust` when the self-hosted configuration allows scripts
- **Count Patchula:** Reports the new tag and opens no pull request. Copier runs only on your own working tree, with your own `copier`, when you ask: a confirmed template in the TUI, or `patchula apply --copier`. Never in a run, never with `--remote` and never on the server
- **Why:** Applying a template runs the template author's code, at the new and unreviewed version. With `--trust` that is tasks, migrations and Jinja extensions; without it, copier's "safe" mode has repeatedly let templates read and write outside the destination directory. On the server that code would run next to the installation token, other repositories' trees, the shared cache and the tool store

The advisories against copier's safe mode:

- [GHSA-3xw7-v6cj-5q8h](https://github.com/advisories/GHSA-3xw7-v6cj-5q8h):
  arbitrary filesystem reads and writes, fixed in 9.9.1
- [GHSA-p7q8-grrj-3m8w](https://github.com/advisories/GHSA-p7q8-grrj-3m8w):
  writes outside the destination path, fixed in 9.9.1
- [GHSA-xjhm-gp88-8pfx](https://github.com/advisories/GHSA-xjhm-gp88-8pfx):
  arbitrary reads through symlinks, fixed in 9.11.2
- [GHSA-4fqp-r85r-hxqh](https://github.com/advisories/GHSA-4fqp-r85r-hxqh):
  arbitrary writes through directory symlinks, fixed in 9.11.2
- [GHSA-9gmc-jqmh-3rvm](https://github.com/advisories/GHSA-9gmc-jqmh-3rvm):
  a trust-prefix bypass that runs tasks unprompted, fixed in 9.15.2

The Count never bumps `_commit` on its own either. Copier treats `_commit` as
the version the project was last generated from, so the next `copier update`
would diff from the wrong base and silently drop the template changes in
between. When he runs copier for you, the Count never passes `--trust`. See
[Copier](/guide/managers/#copier), the [TUI](/guide/tui/#templates) and
[`patchula apply`](/guide/apply/#copier-templates).

### Pins and lockfile refreshes {#update-types}

- **Renovate:** Distinguishes pinDigest and lockFileMaintenance
- **Count Patchula:** Adding a version or digest pin is a `pin` update; lockfile refresh is a separate policy
- **Why:** Keep version classification separate from maintenance

### Cargo compatibility {#cargo-compatibility}

- **Renovate:** Numeric update levels may differ in black-box reports
- **Count Patchula:** A changed pre-1.0 Cargo compatibility boundary is major; exact Cargo uses =
- **Why:** Follow Cargo's documented compatibility rules

Cargo's pre-1.0 boundary follows its leftmost-nonzero rule; npm keeps numeric
SemVer levels. A widened Cargo range is a single interval, since Cargo has no
unions, and a compound range that cannot keep its operators gets a warning.

<details><summary>Comparison harness</summary>

The `cargo-pre1-breaking` rule reclassifies only minor or patch versus major at
those boundaries, with identical targets. It never explains version or range
differences.
</details>

### Minimum release age {#minimum-release-age}

- **Renovate:** The compared run held new releases for roughly 72 to 84 hours, judging from its output
- **Count Patchula:** Follow mise's default minimum_release_age of 24 hours for dated releases; choose the newest eligible release and report held-back versions
- **Why:** A short explicit default with release-date evidence; undated releases have no delay

See [the release-age wait](/guide/lookup-policy/#the-release-age-wait) for how
releases and GitHub tags are dated.

<details><summary>Comparison harness</summary>

A difference is explained when either:

- the Count's target is newer than Renovate's, in the same update class, and
  between 24 and 96 hours old; or
- the Count is still holding the exact target Renovate proposed, inside its
  24-hour window at the time of the lookup.

The matching GitHub Actions digest change is explained on the same terms.
Undated releases, targets older than the window and a changed digest for the
same version are not.
</details>

### npm range edits {#npm-range-edits}

- **Renovate:** The compared policy can retain caret/tilde ranges
- **Count Patchula:** versions = "exact" pins installed dependencies/devDependencies; peers widen ranges or bump exact values
- **Why:** Reproducible installs while preserving consumer support

**Peers are never pinned:** a peer range widens to include the new major
(`^5 || ^7.0.0`), an exact peer is bumped in place, and a satisfying peer range
is left alone by `versions = "exact"`.

<details><summary>Comparison harness</summary>

All of these need an explicit `exact` policy. Differing resolved targets also
need independent release-age evidence.

- **Major:** a numeric caret, tilde or shorthand current range, an exact target,
  and a Renovate caret or tilde range on the target's major line, possibly with
  a lower floor, that contains the target.
- **Minor or patch:** a simple caret or tilde range containing the identical
  target, with the correct update class. An unchanged satisfying Renovate
  declaration paired with the identical exact pin also qualifies.
- **Extra pin:** an existing npm dependency with the same caret or tilde range,
  no Renovate pin, a satisfying exact target and no digest.

Peers never qualify.
</details>

### Pinning floating selectors {#floating-selectors}

- **Renovate:** Leaves some latest selectors without proposals
- **Count Patchula:** Explicit exact policy pins latest to an eligible release
- **Why:** Reproducible toolchains and manifests

<details><summary>Comparison harness</summary>

Explains only extra `pin` proposals for `mise` and `npm` dependencies, or GitHub
setup-action tool inputs with tool-kind and GitHub-release metadata, whose
current value is exactly `latest`, with no Renovate proposals, explicit `exact`
policy, no digest and identical newValue/newVersion.
</details>

### A separate patch track {#patch-track}

- **Renovate:** Often proposes only the highest non-major release
- **Count Patchula:** Offers a patch track alongside a minor track
- **Why:** Let callers choose a smaller upgrade independently

<details><summary>Comparison harness</summary>

Explains an extra patch that is strictly newer than Renovate's resolved current
version, on the same major and minor line, and actually changes the manifest
value. Both sides must have the identical newer minor target on that major line,
and the patch must carry explicit policy metadata. The minor target may differ
only when the release-age rule explains it, or be missing only when the Count
is holding that exact minor inside an evidenced 24-hour window. npm integrity
and Helm archive checksums are ignored as in the main comparison.
</details>

### Go major versions {#go-major-versions}

- **Renovate:** Can report a new numeric major under the old module name
- **Count Patchula:** Go proxy catalogs preserve module identity; /vN migrations are separate
- **Why:** A version-only edit cannot update module paths and imports

The Count does not migrate module paths and imports to a new major.

<details><summary>Comparison harness</summary>

Explains only missing `major` proposals on GitHub-hosted Go modules with
matching identities and the Go datasource on both sides, where the target is a
stable SemVer major of 2 or more without build metadata, newValue equals
newVersion, there is no digest, and the module path lacks the required `/vN`
suffix. `+incompatible` releases are not explained.
</details>

### Cargo no-op proposals {#cargo-no-ops}

- **Renovate:** Can propose a newer satisfying release with unchanged newValue
- **Count Patchula:** Suppress proposals that cannot edit the manifest
- **Why:** Moving the lock to a newer satisfying release is [lockfile maintenance](/guide/lockfiles/#lockfile-maintenance), not a manifest edit

<details><summary>Comparison harness</summary>

Explains only missing version proposals on a Cargo dependency with identical
declarations, Renovate's `crate` and the Count's `crates` source, an unchanged
newValue, no digest, and a stable target that satisfies the original non-exact
requirement.
</details>

### Disabled update types {#disabled-update-types}

- **Renovate:** Lookup snapshots can contain targets disabled by repository rules
- **Count Patchula:** Report effective disabled classes and omit their proposals
- **Why:** Preserve repository policy during comparisons

<details><summary>Comparison harness</summary>

Explains only missing proposals whose exact class is listed in
`disabledUpdates`, on a report with a valid `metadata.lookup.evaluatedAt`.
Nothing is inferred from absent proposals or reports without that evidence.
</details>

### Deprecated and yanked releases {#rejected-releases}

- **Renovate:** Lookup can propose deprecated releases
- **Count Patchula:** Reject deprecated/yanked versions and record the exact rejected targets
- **Why:** Avoid moving users onto withdrawn releases

<details><summary>Comparison harness</summary>

Explains only missing digest-free version upgrades from an npm, crates or PyPI
source, on a report with a valid evaluation time, where the exact version is
listed as deprecated or yanked in `metadata.lookup.rejected`.
</details>

### Stable releases, not LTS {#release-channels}

- **Renovate:** Some Node targets follow the LTS line
- **Count Patchula:** Select the newest eligible stable version allowed by explicit policy
- **Why:** Do not infer a hidden LTS restriction from a package name

For an opt-in Node LTS rule, see
[lookup policy](/guide/lookup-policy/#node-release-channels).

<details><summary>Comparison harness</summary>

Covers mise Node, setup-node inputs and `@types/node`, where the Count picks a
newer stable major while Renovate stays on a lower line. It requires equal
declarations and digests, stable policy, stable ordered majors and digest-free
major proposals. Other packages, peer ranges, same-major drift and explicit LTS
policy are not explained. LTS membership is never guessed from an even major.
</details>

### Action ref precision {#action-ref-precision}

- **Renovate:** Can expand a ref when adding a digest
- **Count Patchula:** Preserve the declared numeric ref precision and report the full resolved target separately
- **Why:** The commit pin supplies reproducibility while the ref describes the intended release line

Adding or updating a commit pin keeps a numeric alias such as `v4`, while
newVersion reports the full target.

<details><summary>Comparison harness</summary>

**Same target:** requires equal package and current identity, github-tags
action or workflow metadata, an unchanged alias with one or two numeric parts,
and the identical full target. The commit must be a 40-digit hash and match
Renovate's when it has one; an added commit is explained under
[explicit defaults](#explicit-defaults).

**Newer target:** both sides propose the same floating tag and update class,
with stable full versions on that line and the Count's newer. The proposed
commit must equal the report's `metadata.tagDigests` entry for that tag. This
covers Renovate pinning an older release's commit.
</details>

### APM commit pins {#apm-commit-pins}

- **Renovate:** Keeps an APM commit pin and its tag comment current only with
  `pinDigests`, which is off unless a preset such as `config:best-practices`
  turns it on
- **Count Patchula:** Pins APM tags to commits by default, through the same
  `actionDigests` default as GitHub Actions, and writes the new tag exactly as
  upstream tagged it
- **Why:** APM packages are instructions an agent follows, so they get the
  same supply-chain default as actions: a commit that cannot move, the
  release-age wait, and no auto-merge

Entries the Count cannot update, such as `owner/repo` with no ref, a branch
or a SHA with no tag comment, are listed under `skipped` in the report rather
than dropped silently. Renovate's docs list `git-tags` for any Git host; the
Count has no generic Git tag source, so hosts other than GitHub, GitLab and
Gitea/Forgejo are skipped.

### Cargo target resolution {#cargo-target-resolution}

- **Renovate:** Observed pre-1.0 upgrades can select an older release in the proposed range
- **Count Patchula:** Resolve the newest eligible release in that same compatibility interval
- **Why:** Report the available release without narrowing the manifest

<details><summary>Comparison harness</summary>

Observed cases: keepass 0.15.0 against 0.15.2, and toml_edit 0.25.16 against
0.25.17+spec-1.1.0, all past both release-age windows, with both sides proposing
the same `0.minor` interval. The rule requires matching declarations and
digests, crate/crates sources, no stale data, a current pre-1.0 minor below both
targets, stable ordered releases on the same nonzero minor, identical proposed
intervals, explicit preserve policy, no digests, and the observed minor-to-major
taxonomy. The missing and extra rows are each audited.
</details>

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

<details><summary>Comparison harness</summary>

Ignores only the proposed newDigest of npm-datasource dependencies, including
regex-extracted npm versions, and of npm or bun manager rows with no source.
Current digests, other sources, targets and update types are still compared.
</details>

### Helm archive checksums {#helm-archive-checksums}

- **Renovate:** Lookup reports chart archive hashes as newDigest
- **Count Patchula:** Helm chart references contain versions, not archive hashes
- **Why:** Artifact verification metadata does not edit the manifest

<details><summary>Comparison harness</summary>

Ignores only the proposed `newDigest` of datasource `helm`. Helm registry URLs
are joined to chart names so both sides use the same registry identity; a
different registry is still a difference.
</details>

### APM lock-only refresh {#apm-lockfile}

- **Renovate:** Refreshes `apm.lock.yaml` by running `apm install`
- **Count Patchula:** Runs `apm lock`, APM's lock-only mode, and `apm lock
  --update` for lockfile maintenance
- **Why:** `apm install` deploys package content into agent directories.
  `apm lock` resolves and hashes the same packages and writes the same lock
  without deploying anything or running package content

The Count keeps only `apm.lock.yaml` from the run; the downloaded
`apm_modules/` tree is discarded. See [APM](/guide/lockfiles/#apm).

### aube for npm and pnpm {#node-lockfile-engine}

- **Renovate:** Regenerate npm and pnpm locks with npm and pnpm
- **Count Patchula:** Regenerate `pnpm-lock.yaml`, `package-lock.json` and `npm-shrinkwrap.json` with aube, which writes the committed format in place; `lockfiles.npmEngine = "npm"` restores native npm for npm lockfiles only
- **Why:** Much faster lockfile updates; aube's npm lockfiles stay valid for `npm ci` but may differ in hoisting and `funding` metadata from npm's own output

Resolved versions are the same; only the lockfile content differs. aube never
adds an `aube-lock.yaml`, and format v1 npm lockfiles always use native npm. See
[why aube](/guide/lockfiles/#why-aube-for-npm-and-pnpm).

## Pull requests and release notes

### Branch and merge safety {#write-safety}

- **Renovate:** No write behavior is inferred from comparison output
- **Count Patchula:** Leased API refs, verified bot ownership, receipt-backed later-run merges when native auto-merge is unavailable
- **Why:** Preserve human work and lockfile consistency without emulating another bot

The Count makes no claim to match Renovate's branch and commit behaviour. Each
update is one GitHub-signed API commit, lockfiles included; branches with human,
unmarked or unverified commits are protected; and atomic ref leases guard the
base and the branch. See [branches and PRs](/guide/pull-requests/), the
[auto-merge fallback](/guide/auto-merge/) and the
[branch actions](/reference/report/#actions) in the JSON report.

### Release notes {#release-notes}

- **Renovate:** No presentation compatibility is claimed
- **Count Patchula:** Ecosystem metadata, public mise-versions note lists, cached changelog sections, bounded ranges, prose-only mention neutralisation and readable references through GitHub's documented backlink-free redirect host
- **Why:** Useful upstream context without steady-run reads or notification/timeline noise; see [release notes](/guide/release-notes/)
