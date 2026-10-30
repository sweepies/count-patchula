---
title: "Image SBOM advisories"
---
When a pinned image's digest is replaced, the Count compares the OS packages
in the old and new digests' SBOMs through OSV.dev. The user-facing behaviour is
in [container image rebuilds](/guide/security/#container-image-rebuilds);
this page covers how it is built.

OSV.dev stays the only vulnerability source. No image layer is ever read, no
package list is stored outside the HTTP cache, and Docker Hub is only read
anonymously.

## Where it runs

The engine's `image` phase runs after digests are resolved and before the
report is assembled, for repositories with `security.advisories` and
`security.images` on. For every pinned Docker or OCI dependency it collects the
current digest and each new digest a proposal will pin (a same-tag digest
update, or a tag update's `newDigest`), and reads one SBOM per digest and
platform in `security.imagePlatforms` (`linux/amd64` by default), 16 at a
time. A dependency with a digest that has none of those platforms is then read
for the first platform that digest's index lists, on all its digests. Only the
digests of a replacement are sent to OSV, in one batched `querybatch` call.

The current digest is read even without a replacement, so the dashboard can
list an image whose SBOM gives nothing to compare before its first rebuild.

## Reading an SBOM

`patchula_datasource::Client::image_sbom` (`crates/datasource/src/images.rs`)
returns `Found`, `Missing`, `NoPlatform` (the index has no such platform; it
carries the first platform the index does list, skipping attestations) or
`Gone` (the registry answers 404 for the digest):

1. GET the manifest by digest and check its SHA-256. An index is searched for
   the platform's manifest and for an attestation manifest whose
   `vnd.docker.reference.digest` is that manifest; its layer with
   `in-toto.io/predicate-type: https://spdx.dev/Document` is the SBOM.
2. Otherwise the cosign tags: `sha256-<digest>.att` for the platform digest,
   then the index digest, then `.sbom` for both. A HEAD comes first, so a
   missing tag costs nothing against pull limits. An `.att` layer must be an
   SPDX DSSE envelope whose in-toto subject is the digest it was found for.
3. Each blob's SHA-256 is checked against its digest.

A Docker official image (`library/*` on Docker Hub) is read from
`mirror.gcr.io` first, and from Docker Hub only when the mirror fails or
answers 404. Because every byte is checked against its digest, the mirror
can't change what is read. Registry tokens, retries and rate limits go
through the same client as every other registry read.

SPDX 2.x is read on its own, in an in-toto statement or in a DSSE envelope
around one. The blob is reduced once, when fetched, by the `sbom-os-v1` derived
representation of the [HTTP cache](/internals/cache/): its digest, in-toto
subjects, generator (SPDX `creationInfo.creators`), the count of OS packages,
the package types and namespace, the distro, and the set of source packages to
query. File entries, which make up most of a Docker Scout SBOM, are never
stored.

## Source packages

OSV's Debian and Ubuntu records name source packages, and Alpine's the origin.
The reducer takes, for each `deb`, `apk` or `rpm` package URL:

- nothing, when the package is the `spdxElementId` of a `GENERATED_FROM`
  relationship, because its source is an entry of its own (Docker Scout);
- the `upstream` qualifier, `name@version` or just `name` (syft);
- otherwise its own name and version.

The distro comes from `os_name` and `os_version`, or from `distro=name-version`.
The Debian source `linux` is dropped: OSV lists every kernel advisory against
it, and a container doesn't run the image's kernel.

Measured against osv-scanner's full image scans, this finds the same OSV IDs on
debian:bookworm, a 2025 bookworm digest, ubuntu:24.04, an older noble digest,
alpine:3.20 and alpine:3.20.3, and on a syft SBOM of a Debian 13 image.

An SBOM is `sources_known` when it has a `GENERATED_FROM` relationship, an
`upstream` qualifier, or comes from Docker Scout. Otherwise it is
`partial-sources`, and its packages are queried under their own names.

## Ecosystems

| Package type and namespace | OSV ecosystem |
|---|---|
| `deb/debian` | `Debian:<major>` |
| `deb/ubuntu` | `Ubuntu:<year>.<month>`, with `:LTS` for even-year April releases |
| `apk/alpine` | `Alpine:v<major>.<minor>` |
| `apk/wolfi`, `apk/chainguard` | `Wolfi`, `Chainguard` |

Anything else, an SBOM mixing package types, and every RPM distribution are
`os-not-covered`. OSV covers some RPM distributions, but their source RPM
names and versions haven't been measured against it yet.

## Comparing

`patchula_lookup::images::compare` works per platform present in both
digests, on the advisory IDs of OSV's batch answer. An advisory is fixed when
OSV lists it for one of the old SBOM's queries and for none of the new one's; its `new` version is the new image's
version of that source package, or none when the package is gone. Advisories
only the new image has are `introduced`. The result's state is the worst of its
platforms, in this order: `unreadable`, `previous-unavailable`, `no-sbom`,
`sbom-without-os`, `os-not-covered`, `partial-sources`, `checked`,
`no-os-packages`. A digest pair with no platform in common is not checked.

An empty OS package list is trusted as "nothing to list" only from a generator
that catalogs whole images: Docker Scout, syft or apko. Any other generator's
empty list is `sbom-without-os`.

A rebuild that fixes something gets the advisory IDs in `fixes`, the policy of
`security.apply` (through `Evaluator::secure`) and `security.label`. The
advisories join `metadata.advisories`, so the dashboard's known
vulnerabilities and the PR's `Change.advisories` carry them; the comparison
itself is `Update.image`, and the current digest's warning, if any, is
`metadata.image`. A warning for an image matched by `security.quietImages`
carries the reason in `silenced`; the PR leaves it out and the dashboard lists
it under "Silenced".

If the OSV call fails, comparisons that would be `checked` or
`partial-sources` become `unreadable`, so a failure never reads as "nothing
fixed".

Not built yet: an update held back by its release-age wait isn't compared, so
an image fix in it doesn't skip the wait the way a package fix does; and there
is no fallback to the distro's signed package index for SBOMs that give no
source packages.

## Cost

The [HTTP cache](/internals/cache/) treats a registry manifest or blob addressed
by digest as fresh for ten years, and stores it even when the registry says
`no-store` or `Vary` (quay.io does), since the bytes are public and checked
against their digest. A 404 for a digest or a `.att`/`.sbom` tag is kept for a
day. A cosign tag's manifest is read by the digest its HEAD names, so only the
HEAD repeats.

The image queries go to OSV in their own batch after the digest phase, because
the new digests are known only then. Advisory records are read only for the
five most severe IDs a replacement fixes and the five most severe it brings in
(`images::SHOWN`), ranked by `images::most_severe` from the ID alone, since
the batch answer has no severity. The PR shows those in its table and lists
the rest as links "listed by OSV". A rebuild is a security fix only when at
least one fixed record read isn't withdrawn; when all of them are, the next
five in rank are read, until one holds or none are left. A withdrawn record
takes its ID out of the comparison.

Measured on five pinned images (`debian:bookworm`, `ubuntu:24.04`,
`alpine:3.20` at older digests, `node:22.11.0-bookworm-slim`, and a quay.io
image whose old digest is gone), on the default `linux/amd64`, with
`security.images` on and off, a cold cache, then warm:

| | Requests, off | Requests, on | Time, off | Time, on |
|---|---|---|---|---|
| Cold | 76 | 165 | 2.6 s | 5.4 to 5.9 s |
| Warm | 12 | 16 | 0.2 s | 1.2 s |

The cold run's extra 89 requests are the manifest, SBOM and cosign reads, with
SBOM blobs redirected from mirror.gcr.io (one Ubuntu SBOM is 2.4 MB, a Debian
one 4.8 MB), about 20 OSV records, at most ten per image update, and the OSV
batch; all but the batch are cached after that. Adding `linux/arm64` costs 42
more cold requests and about a second (207 requests, 6.4 to 7.0 s) for the
same advisories on these images. Reading the record of every advisory the
rebuilds fix or bring in took 521 requests and 8.0 to 9.9 s. The warm run's four
requests are the OSV batch, which takes most of its 0.8 s, and three HEADs for
quay.io cosign tags. The tests corpus has no pinned images the Count extracts,
so it costs the same with the check on or off.

## Tests

- `crates/datasource/src/images.rs` unit tests: syft `upstream` and the
  kernel filter, Scout source entries, ecosystem names, DSSE subjects.
- `crates/datasource/tests/registry_protocol.rs`, simulated: an `.att` for
  another subject is skipped for the `.sbom` tag, a manifest whose bytes don't
  match its digest is rejected, and an official image the mirror lacks is read
  from Docker Hub.
- `crates/lookup/src/images.rs` unit tests: fixed, removed and introduced
  advisories across platforms, state precedence, and which current-digest
  states are warnings.
- `crates/engine/tests/images.rs` replays recordings: two `alpine:3.20` digests
  from mirror.gcr.io and OSV.dev, where the newer one fixes 29 advisories
  (`fixtures/engine/sboms-alpine.json`); and a Chainguard `.att`, a BuildKit
  SBOM without OS packages (`busybox`) and an image without an SBOM
  (`fixtures/engine/sboms-registries.json`).
- `crates/engine/tests/advisories.rs`: a rebuild whose image annotations claim
  a security fix, with no OSV match, gets no fixes and no label, while its twin
  with a match gets both under `security.apply`.
- PR and dashboard wording in `crates/update/src/pull_requests.rs` and
  `crates/update/tests/dashboard.rs`.

Registry bodies are checked against their digests, so the recorder keeps a
body whose SHA-256 is a digest it was asked for (even after a redirect) byte
for byte instead of reformatting it as JSON, and the redactor leaves a URL's
bytes alone unless it redacts something in it.
