# CI cache verification (SWE-81)

Inspected run `38642667666`, attempts 1 and 2, with `mise exec -- gh run view
38642667666 -R sweepies/rustovate --attempt 1 --log` (and attempt 2).
Attempt 1 saved target-cache ID `8963308758`; attempt 2 restored the exact
342 MB archive and skipped re-saving it. Thus the original archive transport
worked. mbx explicitly says its object counters exclude Cargo artifact reuse
and CI archive transfers. Those counters alone did not prove a cache failure.

The workflow now selects `github-cache-mode: objects` and
`isolate-objects-cache: true` on the existing pinned action. It imports portable
objects and prediction/build-script metadata before builds, then exports the
deduplicated closure of every completed command using the action's automatic
`MBX_CACHE_EXPORT_GROUP`. An isolated `RUNNER_TEMP` store avoids unrelated local
state; the action disables automatic GC on hosted runners so imported objects
survive. `v2-objects` starts a deliberate cold generation. No server URL, OIDC
permission or extra GitHub token scope is required for the GitHub backend.
The action controls default-branch publication; PR saving stays disabled.

The first pushed run should report no bundle, then an exported and saved
bundle in its post step. A re-run should report bundle restoration/import and
an exact hit; a subsequent commit should restore by compatible prefix and save
a new entry. Eligible compilations should show object hits; unsupported crate
types and rustdoc can still bypass. Per-command remote byte counters remain
zero because transport happens in the action. Inspect both setup and post logs.

Local `mise run cache:doctor` passes with zero failures (warnings about archive
probing and the global Cargo shim are environment-specific; mise wraps Cargo).
An Actions upload/restore cannot be verified locally without its runtime cache
service; no push or workflow dispatch was performed.

Sources: [GitHub Action documentation](https://mr-boxington.jdx.dev/github-action),
[remote backends](https://mr-boxington.jdx.dev/remote-cache),
[pinned action README](https://github.com/jdx/mr-boxington-action/blob/47ce49b32025d93a7a2f4570815afaa2b7092240/README.md),
[mbx repository CI documentation](https://github.com/jdx/mr-boxington/blob/main/docs/github-action.md).
