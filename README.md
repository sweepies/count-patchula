# rustovate

A fast dependency update bot for GitHub, written in Rust.

rustovate keeps the dependencies of a set of repositories up to date: it finds
dependencies in each repository, looks up newer versions, and opens pull
requests. It is built to run a whole batch of repositories in one short-lived
process, so it is designed around shared caching and concurrency rather than
one repository at a time.

Status: early development (Phase 0). Nothing here is usable yet.

- Design: [docs/design.md](docs/design.md)
- Deliberate departures from Renovate: [docs/departures.md](docs/departures.md)
- Progress: the `rustovate` project in Linear (sweepies team)

## Clean-room rule

rustovate is an independent implementation under the MIT licence. Renovate
(AGPL-3.0) may be run as a black box to compare results, but its source code,
preset data and test fixtures must never be read, copied or adapted into this
repository. Fixtures are recorded from real registries by rustovate itself.
