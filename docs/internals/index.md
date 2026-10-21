# Internals

How the Count is put together, for people working on him. None of this is
needed to use him.

- [Design](/internals/design): the goals, principles and crate layout.
- [Engine](/internals/engine): how a batch moves from extraction to writes.
- [SQLite cache](/internals/cache): HTTP entries, catalogs, freshness and
  host limits.
- [GitHub client](/internals/github-client): REST and GraphQL, conditional
  requests and the rate-limit governor.
- [HTTP fixtures](/internals/http-testing): how tests record and replay
  registries offline.
- [CI cache](/internals/ci-cache): the mbx build cache in GitHub Actions.

The source lives at
[sweepies/count-patchula](https://github.com/sweepies/count-patchula). The
clean-room rule applies to every change: Renovate may be run as a black box,
but its source code, presets and test fixtures are never read or copied.
