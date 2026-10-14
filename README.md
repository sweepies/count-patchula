# rustovate

A fast dependency update bot for GitHub, written in Rust.

rustovate keeps the dependencies of a set of repositories up to date: it finds
dependencies in each repository, looks up newer versions, and opens pull
requests. It is built to run a whole batch of repositories in one short-lived
process, so it is designed around shared caching and concurrency rather than
one repository at a time.

Status: early development (Phase 0). Configuration checking, offline HTTP
fixtures, SQLite caching and the GitHub read client are implemented; dependency
scanning and updating are not implemented yet.

- Design: [docs/design.md](docs/design.md)
- Deliberate departures from Renovate: [docs/departures.md](docs/departures.md)
- Progress: the `rustovate` project in Linear (sweepies team)

## Clean-room rule

rustovate is an independent implementation under the MIT licence. Renovate
(AGPL-3.0) may be run as a black box to compare results, but its source code,
preset data and test fixtures must never be read, copied or adapted into this
repository. Fixtures are recorded from real registries by rustovate itself.

## Development

Install [mise](https://mise.jdx.dev), then run `mise trust` and `mise install`.
All tool versions are pinned in `mise.toml`; Rust uses edition 2024. On x86_64
Linux, the `cc` mise task supplies the pinned Zig C linker, including in
minimal images without system development packages.

- `mise run build` builds the workspace.
- `mise run test` runs offline unit and integration tests (Cargo may download dependencies).
- `mise run lint` checks rustfmt and Clippy with warnings denied.
- `mise run ci` runs lint, test and build in order.
- `mise run fixtures:check` scans all HTTP cassettes for possible secrets (also in CI).
- `mise run fixtures:record` opts into real GitHub recording with an existing
  GITHUB_TOKEN or GH_TOKEN; ordinary tests replay offline. See [HTTP tests](docs/http-testing.md).
- `mise run docs` exports the usage derive definition to KDL and generates
  `docs/cli.md`, `generated/rustovate.1` and bash/zsh/fish completions.

The Rust mise option `mr_boxington = true` wraps Cargo with mbx. Use mise tasks
or `mise exec -- cargo …` so builds share that cache. GitHub CI restores the
mbx GitHub Actions cache on PRs and publishes it on successful pushes to main.
CI explicitly uses isolated `objects` bundles: the action imports the previous
compatible bundle, then exports the closure of all build commands in its post
step. The generated keys include the Rust toolchain and commit, with shared
restore prefixes. An exact-hit re-run restores but does not overwrite its
immutable entry. `mise run cache:doctor` checks the local setup; Actions logs
also show the key and save eligibility. Archive traffic is reported by the
action, not mbx's per-command remote byte counters.
Tak is pinned for later benchmarking; there are no benchmark gates yet.

## Configuration commands

`mise exec -- cargo run -p rustovate-cli -- config check examples/renovate-runner.pkl`
validates a config. `config schema` prints the bundled typed Pkl schema.
See [configuration](docs/configuration.md), [CLI reference](docs/cli.md), and
[twelve repository examples](examples). Dependency lookup and update commands
remain explicit Phase 1 stubs and exit with a runtime error.

Foundation APIs: [SQLite cache](docs/cache.md) and [GitHub client](docs/github.md).

The [comparison harness](conformance/README.md) compares black-box Renovate JSON
logs with the [v1 batch report](docs/report.md). Phase 0 includes a hand-written
sample; dependency lookup is still unimplemented.
