# Renovate comparison (SWE-87)

This workspace binary reads **only Renovate's output**, never its source, preset
data or fixtures. Its fixtures are small JSON-lines examples written by hand;
`demo/rustovate-sample` and `example/lookup` are fictional. The adapter was also
verified against a caller-supplied private Renovate 44.155.6 lookup log; that log
and its triage outputs stay outside this repository.

Run an offline sample:

```sh
mise run conformance:compare --report conformance/tests/fixtures/report.json \
  --renovate-log conformance/tests/fixtures/renovate.jsonl
```

Run the pinned Renovate 44.155.6 executable as a black box against repositories you
can read (install mise tools first). Supply your token through the environment,
never as a CLI argument:

```sh
# RENOVATE_TOKEN is already exported by the caller.
mise run conformance:compare --report /path/to/rustovate-report.json \
  owner/repo-one owner/repo-two
```

The harness installs the exact Renovate package with `mise exec -- bun install
--ignore-scripts` in a temporary directory, then invokes its executable with
mise's Node 24. Bun is only the installer, avoiding Renovate's unsupported-runtime
warning under Bun. It sets `LOG_LEVEL=debug`, `LOG_FORMAT=json`,
`RENOVATE_DRY_RUN=lookup`, GitHub platform, explicit targets, no autodiscovery and
no onboarding. It supplies an empty operator config and temporary base/cache
paths. This is a read-only dry run. Repository configs still influence Renovate's
results; rustovate has its own policy. Temporary installations, clones, caches
and raw logs are removed on normal completion and error. Process termination
(e.g. SIGKILL) can leave OS temporary directories to clean manually. No raw live
log is persisted in the repository or echoed on failure.

You can instead capture JSON debug output elsewhere in lookup or full dry-run
mode and provide it directly:

```sh
mise run conformance:compare --report batch.json --renovate-log renovate.jsonl \
  --output conformance-results owner/repo-one owner/repo-two
```

The mise task uses [`raw_args = true`](https://mise.jdx.dev/tasks/task-configuration.html#raw_args)
to forward arguments, including `--help`, to the binary. For example:

```sh
mise run conformance:compare --help
```

The optional repo list selects an exact subset; a requested repository must exist
on at least one side. Missing repositories/files emit one difference per contained
dependency, including dependencies without updates. Empty files/repositories still
emit coverage differences. An empty rustovate report is valid and makes every
included Renovate dependency `rustovate-missing`:

```sh
mise run conformance:compare --report /path/to/empty.json \
  --renovate-log /path/to/private-renovate.jsonl --output /path/to/private-triage \
  owner/repo-one owner/repo-two
# empty.json: {"schemaVersion":1,"repositories":[]}
```

The parser takes each repository's latest `packageFiles with updates` snapshot.
Real 44.155.6 output puts the manager-keyed object under **`config`**, with an array
of files per manager and a `deps` array per file. File metadata such as `lockFiles`
and `packageFileVersion` is optional. The older hand-written `packageFiles` object
shape is accepted with a warning. Identical occurrences sharing file/manager/name,
package identity and current fields merge, and identical proposals deduplicate.
Conflicting identities fail rather than silently discarding a dependency.

The adapter keeps aliases, current value/version/digest, proposed
value/version/digest, update type and group name. `packageName` defaults to
`depName`; a missing `depName` falls back to `packageName`, with warnings. Skipped
lookups remain dependencies, including entries without current values. It
recognizes `datasource`, `skipReason`, and other lookup metadata without expanding
the v1 report's comparison fields. `skipReason` and upstream warning presence are
reported as diagnostics without copying their payloads. An absent current version
is warned about; absent optional digest/group/target components mean null.

Missing/non-array optional `updates` becomes an empty list with a warning;
malformed optional string fields become null. A missing update type is retained as
`unknown`; proposals with no value/version/digest target are omitted with warnings.
Unknown file/dependency/update fields are ignored with line/field warnings, and
unknown manager/type names remain literal with warnings. Metadata keys observed
in the real output are recognized and ignored deliberately. This is a format
adapter, not a dependency extractor.

Optional `branches[].upgrades` or `branch.upgrades` summaries enrich only
unambiguous extracted dependencies/proposals. Malformed or unmatched/ambiguous
summaries warn and are ignored. A diagnostic `branches` object or a string branch
name is not an upgrade summary. The adapter normalizes `gomod`,
`pip_requirements`, `docker-compose`, `custom.regex` and `pinDigest` to rustovate's
independent vocabulary.

Missing snapshots, invalid JSON-looking lines, absent required repository/manager
map/file path/dependency array/identity, conflicting duplicate identities, logged
errors, and unsupported report versions fail the command. Non-JSON runtime banners
are tolerated; errors never echo raw input. Parsing the supplied real lookup log is
verified; invoking the harness's live installation/execution path is still unverified.

Outputs are `triage.md` and `triage.json`. JSON includes both normalized inputs
using the [v1 batch report](../docs/report.md), plus differences, parser `warnings`
and audited `exclusions`. `normalizations` counts skipped group comparisons,
npm digest normalization and ignored empty-file-only coverage. Markdown includes
these counts, warnings and exclusions too.
The fixed `renovate-config-manager` scope rule excludes Renovate's own configuration
manager (snapshots and upgrade summaries) under [departure 19](../docs/departures.md#departure-19);
rustovate has no equivalent. Every other manager remains in comparison. Classification
is `rustovate-missing`, `rustovate-extra`, `field-mismatch`, or
`deliberate-departure`; mapped entries retain `originalClassification` and a
rule/row/link. Equal updates match as sets; a unique identical target exposes
type/group differences, and unique remaining proposals of the same type are
compared field by field. Ambiguous proposals remain extra/missing.
The command exits 0 after producing triage even when differences exist: this is
report-only. Input or execution failures exit nonzero.

[departures.json](departures.json) is a versioned, deliberately small rules file;
`--rules PATH` selects a replacement. Digest additions for actions/images
are mapped to [departure 7](../docs/departures.md#departure-7). An extra pin must
keep the current value/version, add a digest to an unpinned dependency and have
no group. Release-age differences map to [departure 23](../docs/departures.md#departure-23)
only when timestamp evidence places a newer rustovate target between 24 and 72
hours old under an explicit 24-hour policy, with an older Renovate proposal in
the same class. Exact npm major range edits map narrowly to
[departure 25](../docs/departures.md#departure-25) with identical resolved targets
and an explicit exact policy. Other version and identity differences remain actionable.

The reverse age difference is also departure 23 when Renovate's newer target
exactly matches a rustovate `metadata.heldBack` version in the same update class.
The paired rustovate proposal must carry a 24-hour policy and an evaluation
timestamp inside that held-back release's publication/eligibility interval,
which must be exactly 24 hours. Future publication dates, expired eligibility,
different versions/classes and missing evidence stay actionable. A `newValue`
difference additionally requires exact values on both sides; this rule does not
explain range edits. Held-back versions use the same extracted version identity
as proposals, including when an extractor removes a tag prefix.

Proposed npm integrity digests are ignored under
[departure 24](../docs/departures.md#departure-24), including regex dependencies
whose datasource is npm. Other-source and current digests remain comparable.
Groups compare only when Renovate supplies a group on that update, including
enrichment from unambiguous branch summaries. Missing group metadata is unknown.
Empty package-file-only differences are ignored when no dependency is missing;
missing repositories and dependencies still produce coverage differences.
Rule validation checks row IDs against the bundled departures document. Adding
new predicates requires reviewed Rust code, not unrestricted matching rules.
Markdown links resolve to the local departures document; JSON keeps portable
repository-root links. Do not commit captured logs: they can contain credentials.
