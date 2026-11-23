---
title: "The Count's instance"
description: "How the release workflow keeps the project's own server on exactly the newest release tag."
---

The project runs its own `patchula serve` instance. It must never be older
than the newest release: once a `v*` tag exists, no earlier Count may run a
batch, because that batch would sweep repositories against the new release.

## How a release moves it

`release.yml` runs `mise run instance:deploy` twice for each tag:

1. **Pause**, as soon as the tag is pushed and before anything is built: the
   Deployment is applied at that tag with 0 replicas. The running server gets
   SIGTERM and drains, so nothing sweeps while the release builds.
2. **Deploy**, after the image is published: the Deployment is applied at
   `ghcr.io/sweepies/count-patchula:X.Y.Z@sha256:…`, the digest that release
   pushed, with 1 replica.

Both steps do nothing unless the tag is the newest `v*` tag on GitHub (a
prerelease sorts below its release), so a rerun of an old release never
brings an old Count back. Deploys run one at a time. A deploy that finds a
newer tag after it applied pauses the instance again. If a release fails
after its pause, the instance stays at 0 until a release succeeds.

`instance/alchemy.run.ts` declares the Deployment and Alchemy applies it
with server-side apply. No state is kept between runs: every run applies
the whole object, which creates it or converges it.

## What the cluster provides

The Deployment is all the workflow writes. The namespace (`INSTANCE_NAMESPACE`,
`count-patchula` by default) must already hold:

| Object | Name | Contents |
|---|---|---|
| Secret | `count-patchula` | `PATCHULA_APP_ID`, `PATCHULA_APP_PRIVATE_KEY`, `PATCHULA_WEBHOOK_SECRET` |
| ConfigMap | `count-patchula-settings` | `settings.pkl`, mounted over `/etc/patchula` |
| PersistentVolumeClaim | `count-patchula-data` | mounted at `/var/lib/patchula` |

Pods are labeled `app.kubernetes.io/name: count-patchula` and name their
port `http`, for a Service to select. They run only on nodes labeled
`count-patchula.sweepy.dev/instance=true` and tolerate a `NoSchedule` taint
with that key. They resolve names with the node's own resolvers
(`dnsPolicy: Default`), not cluster DNS.

The workflow signs in with the job's GitHub OIDC token, minted for the
audience in `INSTANCE_AUDIENCE`. The cluster must accept that issuer and
audience, admit only this repository's `release.yml` on `v*` tags, and let
that identity manage Deployments in the namespace.

## Repository variables

| Variable | Value |
|---|---|
| `INSTANCE_ENDPOINT` | The API server URL, reachable from GitHub's runners |
| `INSTANCE_CA` | The API server's CA bundle, base64-encoded PEM |
| `INSTANCE_AUDIENCE` | The OIDC audience the cluster accepts |

Without `INSTANCE_ENDPOINT` both jobs are skipped, so forks release as before.
