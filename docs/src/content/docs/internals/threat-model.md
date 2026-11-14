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
- **A trusted repository** matches `trustedRepositories` in the operator
  settings. Its writers count as the operator. A local directory scan is the
  operator's own checkout and is trusted too.
- **An untrusted repository** is any other repository the Count reads.
  Treat its writers as possibly hostile.

Someone who can change what the Count reads in a repository — its
configuration, manifests or lockfiles on the branch he reads — already has
write access to that repository. Nothing the Count does can take that access
away, and nothing needs to protect the repository from its own writers.

## What an untrusted repository may cause

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
  App key, another repository's token, or an operator registry credential
  meant for other repositories.
- see data that only another repository's credentials could fetch. Results
  shared between repositories, such as listings and catalogs, are shared only
  when they came from public data or from a credential the reader may hold.

## What a trusted repository may cause

Anything the operator may. Trusted means trusted: it gets the run's
credentials, and code-running paths are allowed for it. There are no extra
guards against a trusted repository, including in caches it shares with
others.

## Consequences

- **Follow, don't refuse.** If the Count can reproduce what a repository
  declares, he does it. He refuses only what would run code for an untrusted
  repository, or what he cannot reproduce faithfully.
- **Scope credentials, don't strip them.** Give each lookup the narrowest
  credential its repository is entitled to. Don't withhold that credential
  because a repository-chosen host could see it.
- **Don't run code from untrusted repositories.** Every code-running path
  checks the repository's trust. Untrusted means off.
- **Keep shared results honest.** A shared cache key includes everything that
  changes the answer: the options, the source, and the reach of the
  credential that fetched it.
- **Argument hygiene is separate from trust.** Rejecting values that a tool
  would read as flags, selectors or extra arguments is about parsing
  correctly, and applies to every repository.

## Out of scope

- Protecting a repository from its own writers.
- Sandboxing a trusted repository's code.
- A compromised operator, or a compromised host the operator configured.
