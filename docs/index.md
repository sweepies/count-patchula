---
layout: home
title: Count Patchula
titleTemplate: Dependency updates for GitHub

hero:
  name: Count Patchula
  text: Vants to update your deps.
  tagline: He never sleeps. Neither do your CVEs. A modern dependency updater with strong defaults.
  image:
    src: /logo.png
    alt: Count Patchula, a bat holding a wrench
  actions:
    - theme: brand
      text: Get started
      link: /getting-started
    - theme: alt
      text: Coming from Renovate
      link: /renovate/compatibility
    - theme: alt
      text: GitHub
      link: https://github.com/sweepies/count-patchula

features:
  - title: One run, every repository
    details: Repositories are scanned concurrently, and each package is looked up once per batch however many repositories use it. Unchanged work is skipped through validators and receipts.
    link: /guide/how-it-works
    linkText: How it works
  - title: Real lockfiles
    details: npm, pnpm, Yarn, Bun, Cargo, Go, Terraform, uv, Poetry, NuGet, Bundler, Composer and mise locks are regenerated with the real tools, installed by mise into a store the Count keeps between runs.
    link: /guide/lockfiles
    linkText: Lockfiles
  - title: Typed configuration
    details: Policy is written in Pkl against a published schema. Unknown fields are errors with a file and line, not a silently ignored typo.
    link: /guide/configuration
    linkText: Configuration
  - title: Batch it yourself
    details: Tick a box on any update PR and the Count moves it into one batch PR with the version you reviewed, then closes the original. Untick to send it back.
    link: /guide/batch
    linkText: Batching
  - title: Built in Rust
    details: One native binary. Against Renovate 44.173.1 on six large public repositories, the Count was 6.1 times faster cold and 18.5 times faster warm, using a fraction of the memory.
    link: /benchmarks
    linkText: Benchmarks
  - title: The server is MIT too
    details: The GitHub App server, with webhooks, scheduled sweeps and warm caches, ships in the same MIT-licensed binary as the CLI. No paid edition, no feature gates, no licence key.
    link: /guide/server
    linkText: Server mode
---

## What it does

Point the Count at some repositories with a token and a small Pkl file. He reads
their manifests through the GitHub API, looks up newer versions, and opens one
pull request per update, each with a regenerated lockfile, release notes and a
verified commit signed by GitHub.

```sh
patchula run --dry-run sweepies/my-app   # look first
patchula run sweepies/my-app             # then let him bite
```

```pkl
// .github/patchula.pkl
amends "../Patchula.pkl"

prs { dashboard = true }
rules {
  new Rule {
    select { updates { "patch" } }
    apply { merge = true }
  }
}
```

Supported ecosystems include npm, pnpm, Yarn and Bun, GitHub Actions, mise,
Cargo, Go, Docker and Compose, Kubernetes, Kustomize, Flux and Helm, Terraform,
Python (requirements, PEP 621, uv and Poetry), Maven and Gradle, NuGet, Bundler,
Composer, pre-commit and devcontainers. The [compatibility
matrix](/renovate/compatibility) lists what is covered and what is not.
