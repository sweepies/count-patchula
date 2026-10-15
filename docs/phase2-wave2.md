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
