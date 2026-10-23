# patchula-mise

Tool versions for the `mise` source, listed by the Count's pinned
`mise ls-remote --json` in a sandbox (`Lookup`), with the rules for which tool
names it accepts (`check`) and the environment mise runs in (`sandbox_env`,
shared with the lockfile runner).

`registry.json` holds shorthand/backend identifiers from `mise registry --json`
using mise 2026.10.8 on 2026-10-15. Source:
[jdx/mise registry](https://github.com/jdx/mise/tree/main/registry), MIT licensed.
It is used for one thing: telling which GitHub repositories the mise-versions
mirror lists (`listed_github_repository`), so that plain GitHub lookups read the
mirror only where it has data. Tool lookups never read it. Refresh with
`mise run registry:update`; review the data diff and run CI.
No plugin code is read or run during dependency extraction or lookup.
