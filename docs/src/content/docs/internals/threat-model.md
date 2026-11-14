---
title: "Threat model"
description: "Who the Count trusts, what a repository may cause, and which guards follow from that."
---
Read this before changing credentials, lookups, caches shared between
repositories, or anything that runs a tool. Every guard in those areas should
follow from the rules below. A guard that doesn't follow from them is noise,
and it breaks legitimate setups.

## Who is trusted

- **The operator** runs the Count and writes its settings: the operator
  settings file, flags and environment. The operator is fully trusted.
- **Every repository** the Count reads is treated the same way. Its writers
  may be hostile toward the Count and toward other repositories. There is no
  setting that trusts a repository.

Someone who can change what the Count reads in a repository — its
configuration, manifests or lockfiles on the branch he reads — already has
write access to that repository. Nothing the Count does can take that access
away, and nothing needs to protect the repository from its own writers.

## What a repository may cause

It may:

- point the Count at any host, registry or package its configuration names.
  The Count follows repository-declared sources, such as a registry, a mise
  `api_url` or tool options. He never falls back to a public package of the
  same name when the declared source fails (dependency confusion).
- receive a credential whose reach is no wider than that repository's own,
  such as a token minted for that repository. Sending that credential to a
  host of its choosing gains the writer nothing.

It may not:

- run code on the Count's machine. That covers plugin backends (asdf, vfox),
  lifecycle scripts, hooks, build logic, credential helpers and anything else
  that executes something the repository chose. The Count runs as one user,
  with his credentials, his cache and, under an App, the private key, so code
  running there reaches all of them.
- obtain a credential wider than its own reach: the installation token, the
  App key, another repository's token, an operator registry credential meant
  for other repositories, or the machine's own credentials. Cloud
  instance-metadata endpoints (link-local addresses such as `169.254.169.254`)
  hand those out, so a repository-chosen fetch target may not be one.
- see data that only another repository's credentials could fetch. Results
  shared between repositories, such as listings and catalogs, are shared only
  when they came from public data or from a credential the reader may hold.

## Consequences

- **Follow, don't refuse.** If the Count can reproduce what a repository
  declares, he does it. He refuses only what would run code chosen by the
  repository, or what he cannot reproduce faithfully.
- **Scope credentials, don't strip them.** Give each lookup the narrowest
  credential its repository is entitled to. Don't withhold that credential
  because a repository-chosen host could see it.
- **Never run code a repository chose.** Plugin backends, lifecycle scripts,
  hooks and the like stay off for every repository.
- **Keep shared results honest.** A shared cache key includes everything that
  changes the answer: the options, the source, and the reach of the
  credential that fetched it.
- **Argument hygiene is separate from trust.** Rejecting values that a tool
  would read as flags, selectors or extra arguments is about parsing
  correctly, and applies to every repository.

## Out of scope

- Protecting a repository from its own writers.
- A compromised operator, or a compromised host the operator configured.
