# Renovate comparison (SWE-87)

This workspace binary reads **only Renovate's output**, never its source, preset
data or fixtures. Its fixtures are small JSON-lines examples written by hand;
`demo/rustovate-sample` is fictional. No private repository has been queried.

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

The optional repo list selects an exact subset and requires it on both sides.
Without a list, differences in repository/file coverage are included, even for
empty repositories. Missing snapshots, malformed comparison data, logged errors,
unsupported report versions and ambiguous identities fail the command. The
parser tolerates non-JSON runtime banners but never echoes raw input. Live
execution and compatibility with real 44.155.6 output are not yet verified;
parser tests currently cover the hand-written shapes only.

The parser takes each repository's latest `packageFiles with updates` snapshot
(manager-keyed files with `deps` and `updates`) and enriches updates from
`branches[].upgrades` or `branch.upgrades` summaries when present. Summaries need
an unambiguous extracted dependency. It keeps package aliases, current value,
version/digest, proposed value/version/digest, update type and group name. It
normalizes `gomod`, `pip_requirements`, `docker-compose`, `custom.regex` and
`pinDigest` to rustovate's independent vocabulary. Unknown manager/type names
remain literal. This is a format adapter, not a dependency extractor.

Outputs are `triage.md` and `triage.json`. JSON includes both normalized inputs
using the [v1 batch report](../docs/report.md), plus differences. Classification
is `rustovate-missing`, `rustovate-extra`, `field-mismatch`, or
`deliberate-departure`; mapped entries retain `originalClassification` and a
rule/row/link. Equal updates match as sets; a unique identical target exposes
type/group differences, and unique remaining proposals of the same type are
compared field by field. Ambiguous proposals remain extra/missing.
The command exits 0 after producing triage even when differences exist: this is
report-only. Input or execution failures exit nonzero.

[departures.json](departures.json) is a versioned, deliberately small rules file;
`--rules PATH` selects a replacement. Only digest additions for actions/images
are mapped to [departure 7](../docs/departures.md#departure-7). An extra pin must
keep the current value/version, add a digest to an unpinned dependency and have
no group. Other version, identity and grouping differences remain actionable.
Rule validation checks row IDs against the bundled departures document. Adding
new predicates requires reviewed Rust code, not unrestricted matching rules.
Markdown links resolve to the local departures document; JSON keeps portable
repository-root links. Do not commit captured logs: they can contain credentials.
