# rustovate design

## Goal

Replace the Renovate CLI in `sweepies/renovate-runner` with a tool that updates
the dependencies of about a dozen repositories in seconds, not minutes.

Measured baseline (Renovate 44.155.6, warm caches, one EC2 c7i.large):

| | Time |
|---|---|
| Batch of 12 repos, before our release-notes patch | 6.7 min |
| fnox alone, after the patch | 44 s |
| renovate-runner alone, after the patch | 13 s |

Where Renovate's time went: one repository at a time, per-repository caches,
full release and tag histories fetched through paginated APIs, every changelog
re-parsed per version, and Node startup per process. The remaining floor is
GitHub round-trips and lockfile tools (`bun`, `npm`, `cargo`, `go`), which no
language change removes.

Targets (to be confirmed by Phase 1 measurements):

- Warm batch, nothing changed: under 5 s of rustovate time for 12 repos.
- Warm batch with updates: 10–30 s, bounded by lockfile tools and GitHub writes.

## Principles

1. **Batch-native.** One process handles the whole batch. Repositories are
   processed concurrently (default 8) behind one GitHub rate-limit governor.
   Each lookup (data source + package) runs at most once per batch, however
   many repositories use the package.
2. **Incremental by default.** Release lists, tag lists and changelogs are
   stored with their validators (ETag, Last-Modified) and the newest item seen.
   A refresh fetches newest-first and stops at the first item already known.
   Nothing paginates a full history after the first fetch.
3. **Conditional everywhere.** Every GET is conditional when a validator
   exists. GitHub does not count 304 responses against the primary rate limit.
4. **No clone unless needed.** Repository files are read through the GitHub
   API at the default branch's commit. Commits are created through the API
   (signed by GitHub). A working tree is materialised from a persistent partial
   clone only when a lockfile tool must run.
5. **Skip unchanged work.** If the default branch's commit and the resolved
   config are unchanged, the previous extraction is reused. If no new versions
   appeared either, no branch work happens.
6. **Fail loudly.** Unknown config fields, unknown managers and unsupported
   files are errors with precise diagnostics, never silently ignored.
7. **Structured output.** A stable, versioned JSON report is the interface to
   callers such as renovate-runner. Logs are for humans and never parsed.
8. **Clean-room and MIT.** See the README.

## Configuration

Pkl only, evaluated in-process with `pklr`. rustovate publishes a typed
`Rustovate.pkl` schema; a repository's config amends it, and an organisation
can share a base config through Pkl imports. There is no `renovate.json`
reader and no migration tool: repositories are migrated by hand or by agents.

The schema is designed from scratch around the concepts our repositories use:
rules matching packages, managers, files and update types; grouping; pinning
of versions and digests; labels; PR limits; regex managers; release notes.

## Architecture

```
crates/
  config/       Pkl schema, evaluation (pklr), typed model, rule engine, templates
  versioning/   semver (npm and Cargo), Docker tags, PEP 440, Go modules, HashiCorp, loose
  datasource/   trait + implementations; all HTTP through github/ and cache/
  manager/      per-ecosystem extract and update, regex managers
  lookup/       batch-wide deduplicated lookups, update classification, grouping
  update/       branches, API commits, PRs, dashboard, lockfile-tool runner (Phase 2)
  github/       REST + GraphQL client, conditional requests, rate-limit governor
  cache/        SQLite: HTTP entries, release lists, extraction fingerprints
  cli/          `rustovate` binary, defined with usage
conformance/    Renovate comparison harness (black box only)
```

Async runtime: tokio. HTTP: reqwest with HTTP/2 and rustls. Errors and
diagnostics: miette. Interactive prompts: demand, only on interactive terminals.

## Scope for the first release

Managers: npm and bun (workspaces, `bun.lock`, `package-lock.json`), GitHub
Actions (with digest pinning), mise (`mise.toml`, `.tool-versions`), Cargo,
Dockerfile and Compose, Go modules, Terraform (with its lock file), Kubernetes,
kustomize, Flux and Helm values, pip requirements, regex managers.

Data sources: npm registry, GitHub releases and tags, Docker registries
(Docker Hub, ghcr.io), crates.io sparse index, Go module proxy, Terraform
registry, PyPI, Helm and OCI repositories, and mise's tool registry.

Platform: GitHub only, authenticated by a token the caller provides.

Out of scope: other platforms, onboarding PRs, hosted services, Docker-based
tool installation, arbitrary post-upgrade commands, encrypted config.

## Tooling

All tools come from mise. Builds go through mr-boxington (`mbx`) locally and in
CI. The CLI is defined with usage, which also generates completions, Markdown
docs and the man page. Performance is tracked with tak, report-only until tak
stabilises. Tests never touch the network: they replay responses recorded by
rustovate itself.

## Phases

0. Foundations: workspace, CLI, Pkl config, GitHub client, cache,
   record/replay, comparison harness, benchmarks.
1. Read-only: `rustovate lookup` produces a dry-run report for all 12
   repositories, triaged against Renovate's dry-run output.
2. Write side: branches, commits, PRs, dashboard, lockfile tools.
3. Shadow mode next to Renovate in renovate-runner, then cutover.
