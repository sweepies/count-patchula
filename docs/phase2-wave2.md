# Phase 2, wave 2

## Branch model

Non-major proposals (minor, patch and refresh of an existing digest) use one
`rustovate/<package-slug>-<current-major>.x` branch. The best compatible proposal
per occurrence wins, avoiding overlapping edits. Major proposals retain only the
newest eligible major and use `<new-major>.x`. Zero majors retain `0.x`.
All initial version/digest pins share `rustovate/pin-dependencies`, including
pins otherwise assigned to a configuration group. Groups use `<group-slug>` and
`major-<group-slug>`. The configured `prs.branch` prefix replaces `rustovate/`.

Package/source/line identities combine occurrences across files and container
managers. Equal readable slugs gain manager prefixes (`docker-`, `mise-`, etc.).
A remaining ambiguity fails planning before any writes; explicit groups resolve
it. Container variants retain a suffix (e.g. `3.x-slim`); nonnumeric tracks use
their literal slug (e.g. `latest`). Names contain no identity hashes. Ownership
continues to come from verified, bot-authored commit trailers and complete
inspectable history, never from a name alone.

Before applying current plans, a real run inventories all branches, validates
obsolete marked histories, and deletes obsolete owned heads with atomic head and
base leases. This includes wave 1 hash-suffixed branches even if their old
identity differs. Human, unsigned, unmarked and incomplete histories survive.
Dry runs compute only; cleanup discovery and write precondition reads run only
on real runs.

These names follow the owner's observed Renovate output. Deliberate departures:
a numeric container variant uses the major plus variant, pins take precedence
over config groups, and a residual slug collision fails safely instead of adding
a hash. No Renovate source, presets or fixtures were consulted.

## Write performance

Wave 1 live evidence: `sandbox-run.err` reports write=75.475s, total=77.605s,
230 batch requests; `sandbox-dry.err` reports write=33.587s, total=36.790s,
170 batch requests. These are supplied measurements, not reruns by this agent.

The old writer issued four identical base precondition reads for every plan,
then a branch lookup, one blob POST per changed file, tree/commit POSTs and a
leased ref mutation. Dry runs performed all precondition and ownership reads.
The new dry writer sends **zero requests**. Real writes validate the base once,
run at most four independent branch tasks, and send only changed files inline in
one Git tree POST, followed by commit and ref mutation. A one-file new branch
therefore costs `4 + 4*N` requests per repository instead of `9*N`, excluding
cleanup/PR lifecycle. Existing-branch signature/history verification remains.

Offline trace evidence (`shared_base_write_trace_and_request_budget`): twelve
branches, 150ms synthetic latency/request, 1s mutation cooldown: **52 requests,
41.150s simulated time**; dry run **0 requests, 0s simulated time**. The trace
prints method, host and path, verifies exactly one base tree read, and rejects
blob POSTs. This is a bounded deterministic API simulation, **not a measured
live speedup**. At 12 branches the prior request count was 108. The supplied
22-branch live timing cannot be directly compared to this simulation.

GitHub's [secondary limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api#about-secondary-rate-limits)
include 80 content requests/minute and 500/hour, and its
[best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api#pause-between-mutative-requests)
recommend serialized requests and a one-second pause after mutations. Client
clones now share a mutation lock, one-second post-response pause, rolling hourly
budget and primary/secondary cooldown. Reads remain bounded by the governor.
GraphQL queries do not consume the mutation budget. Mutation failures still
avoid blind retries except for explicit rate-limit refusals. Migration adds
one-time history inspection and ref deletion costs; PR creation adds its own
paced mutations. Do not promise a shorter first migration run than wave 1.

This session has no configured `RUSTOVATE_TOKEN`, `GITHUB_TOKEN` or `GH_TOKEN`.
Fresh live `-vv` before/after traces and write timings require the caller's App
installation token; no credential files were opened to recover one.
