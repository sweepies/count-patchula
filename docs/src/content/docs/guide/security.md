---
title: "Security advisories"
---
The Count checks every dependency he can identify exactly against
[OSV.dev](https://osv.dev), the open vulnerability database that collects the
GitHub Advisory Database, RustSec, the Go and PyPI advisory databases and
others. An update that moves a dependency off a vulnerable version is marked as
a security update everywhere it shows up.

## What gets checked

A dependency is checked when OSV covers its ecosystem and the Count knows the
exact version in use:

| Ecosystem | Packages |
|---|---|
| npm and Bun | registry packages, at the lockfile version or an exact declaration |
| Cargo | crates, at the `Cargo.lock` version |
| Go | modules, and the standard library at the `toolchain` line's version |
| PyPI | packages, at the lockfile version or an `==` pin |
| Maven, NuGet, RubyGems, Packagist | packages with an exact or locked version |
| GitHub Actions | actions pinned to a full version, such as `@v4.2.2` or a SHA with a `# v4.2.2` comment |

A range without a lockfile is not checked, because its floor is not what is
installed. Neither are floating action tags like `@v4`, mise tools or
Terraform providers, which OSV does not cover. Pinned container images are
checked through the OS packages their SBOMs list; see
[container image rebuilds](#container-image-rebuilds).

Each run sends one batched query to OSV with the package names and versions,
and nothing else from your repositories. Advisory details are kept in the cache
and fetched again only when OSV reports them modified, so a warm run costs one
request. If OSV cannot be reached, the run carries on without advisories and
the report says why in a warning.

## Lockfile-only packages

Most vulnerable code arrives through dependencies of dependencies, which only
a lockfile records. The Count checks every registry package in `Cargo.lock`
and `package-lock.json` too. A lockfile-only package that turns out to be
vulnerable shows up in the report, the dashboard and the TUI as a dependency of
kind `transitive` on its lockfile. Packages without a known vulnerability stay
out of the report.

For Cargo, the Count also fixes it: the update targets the lowest fixed release
that fixes the most advisories without crossing a breaking version, and the
branch changes only `Cargo.lock`, with
`cargo update -p <crate>@<current> --precise <fix>`. The new lock must record
the fix, or nothing is published. A fix that only a breaking release provides
needs the crate that depends on it to move first, so it is reported but not
proposed.

npm locks have no tool that moves one nested package to an exact version, so
for now a vulnerable lockfile-only npm package is reported with the release
that fixes it, and the fix is up to you, or to updating whatever depends on it.

Rules select these packages with `kinds { "transitive" }`. For example,
`apply { active = false }` reports them without proposing fixes.

## What a security update looks like

An update fixes an advisory when the current version is affected and the
target is not, judged from the advisory's affected ranges for that package. An
advisory whose ranges the Count cannot evaluate is never claimed as fixed.

- **Pull requests** open with a table of the advisories they fix, with their
  CVE IDs, severity and summary, and the commit message ends in `[security]`.
- **Labels:** the `security` label joins the update's other labels.
- **The dashboard** lists every known vulnerability first, with the update that
  fixes it or the release that would.
- **The report** lists the advisories affecting each dependency under
  `metadata.advisories`, and the ones each update fixes under `fixes`. See the
  [report reference](/reference/report/).
- **The terminal UI** marks security updates in red and names what they fix.

## Container image rebuilds

A pinned image, such as `FROM debian:bookworm@sha256:…`, gets a new digest when
its publisher rebuilds it, usually to pick up patched OS packages. OSV.dev has
no advisories for images themselves, so the Count compares what is inside them:
he reads the SBOM (software bill of materials) the publisher attached to the old
digest and to the new one, and asks OSV.dev about the OS packages each lists.
An advisory OSV reports for the old image and not for the new one is fixed by
the rebuild.

This runs whenever a pinned digest is replaced: a same-tag digest update, and a
tag update of a pinned image. A first pin has nothing to compare against.

**Where SBOMs come from.** The Count reads, by digest and without logging in:

- the BuildKit attestation in the image index, which is where Docker official
  images, and most images built with `docker buildx --sbom`, keep theirs;
- otherwise a cosign attachment: the `sha256-<digest>.att` attestation, whose
  in-toto subject must be the image's digest, or the `sha256-<digest>.sbom`
  tag, for the platform's digest first and then the index's.

Every manifest and SBOM is checked against its SHA-256 digest. Docker official
images are read from `mirror.gcr.io`, Google's mirror of Docker Hub, so they
don't count against Docker Hub's anonymous pull limit; when the mirror lacks a
digest, Docker Hub is asked. The Count never downloads image layers.

**What is asked.** Debian, Ubuntu, Alpine, Wolfi and Chainguard packages
(`deb` and `apk` in the SBOM's package URLs). OSV files Debian and Ubuntu
advisories under source packages, such as `openssl` for `libssl3`, so the Count
asks about the source package each binary package comes from. Docker Scout's
SBOMs list source packages as entries of their own, and syft's name them in an
`upstream` qualifier. When an SBOM gives no source at all, packages are asked
about under their own names and the PR says so. The kernel source package
(`linux`, from `linux-libc-dev`) is left out, because a container never runs the
image's kernel.

Images built on RPM distributions, such as Red Hat UBI, AlmaLinux, Oracle
Linux or Amazon Linux, are not checked yet and are listed as not covered.

**What a fix looks like.** A rebuild that fixes an advisory is a security
update like any other: the PR opens with a table of the five most severe
advisories it fixes, with the package's old and new version (or "removed") and
the platforms, then a count and links for the rest OSV lists. The commit ends
in `[security]`, the `security` label is added and `security.apply` applies.
When `security.apply` turns off updates or digests for the image, or its
`permitted` filter doesn't match the new tag, the rebuild is not a security
update, as for any other dependency; the PR still shows the comparison.
Advisories the new image brings in are listed the same way. Only an OSV record
the Count has read, and that isn't withdrawn, marks a rebuild as a security
fix: not release notes, image labels or annotations. When every record he
reads is withdrawn, he reads the next ones until one holds or none are left.

OSV's batch answer carries no severity, so "most severe" is judged from the
advisory ID: a distro's own security notice (DSA, DLA, USN and the like) ranks
above a per-CVE tracker entry such as `DEBIAN-CVE-…`, and a newer number above
an older one.

**When the Count can't tell.** He says so on the PR:

| State | Meaning |
|---|---|
| `checked` | Both SBOMs read; the PR lists any fixes, or says there are none |
| `partial-sources` | As `checked`, but the SBOM names no source packages |
| `no-os-packages` | The SBOM comes from Docker Scout, syft or apko and lists no OS packages |
| `no-sbom` | No SBOM for the old or the new digest (a warning) |
| `sbom-without-os` | The SBOM lists no OS packages, and its generator doesn't look for them (a warning) |
| `previous-unavailable` | The old digest, or its SBOM, is gone from the registry (a warning) |
| `os-not-covered` | The OS is one OSV.dev has no advisories for (a warning) |
| `unreadable` | A transient failure, such as a registry rate limit; retried next run |

The dashboard lists every pinned image whose current digest is in a warning
state under "Images the Count can't vouch for", so a gap shows before the first
rebuild. The update is still proposed either way.

SBOMs are per platform. By default the Count compares `linux/amd64` only;
add `linux/arm64` or any other platform to `security.imagePlatforms` to compare
it too, where both digests have it. With several platforms the state is the
worst of them, and a fix on any counts. An image with none of the listed
platforms, such as an arm64-only build, is compared on the first platform its
index lists instead, on both digests.

**Cost.** Manifests and SBOMs are addressed by digest, so each is read once
and kept in the cache; of an SBOM only the OS packages are kept. A digest or
cosign tag the registry lacks is asked about again a day later. A warm run
costs one batched OSV query for all images, sent once the new digests are
known, and a HEAD per cosign attestation tag, since those tags can move. OSV's
full record is read for the five most severe advisories a rebuild fixes and the
five most severe it brings in, so the PR's table and its list of introduced
advisories carry them; later ones are read only when every fix read so far is
withdrawn. Each
platform in `security.imagePlatforms` adds its own manifest and SBOM reads on
the first run; on five common images, adding `linux/arm64` took the first run
from 165 to 207 requests.

## Security updates go first

`security.apply` holds policy overrides, like a rule's `apply`, for any
candidate version that fixes an advisory. They are applied after your rules.
The defaults are:

- **No release-age wait:** a fix published an hour ago is proposed now. Releases
  that fix nothing still wait as usual.
- **No schedule windows:** the PR opens on the next run, whatever `windows` say.
- **A PR of its own:** the update leaves its group, so one fix does not wait on
  the rest of the group.

Whatever `apply` says, a PR with a security update never waits for
`prs.concurrent` or `prs.perHour`, and a vulnerable package's release list is
read fresh rather than reused under the release-age wait.

Other overrides work too. For example, `apply { active = true }` lets a fix
through even for an update type your rules turned off, and
`apply { merge = true }` merges fixes once checks pass.

## The lowest fix

Your rules may hold every fix back: a `permitted` filter that stops below the
fixed release, or an LTS `channel` that the fix is not on. When no proposal
fixes as many advisories as some release could, the Count also proposes the
lowest release that fixes the most, and ignores `permitted` and `channel` to do
it. `active` and the release-age wait from `security.apply` still apply.

Say `permitted = "semver:<4.17.21"` keeps lodash at 4.17.20, which fixes one of
your two advisories, and 4.17.21 fixes both. The Count proposes 4.17.21. In the
same update track, the update that fixes more wins the branch. If the newest
permitted release already fixes everything, nothing extra is proposed.
`lowestFix = false` turns this off.

## Configuration

```pkl
security {
  advisories = true    // check dependencies against OSV.dev
  label = "security"   // added to PRs that fix an advisory; "" adds none
  apply {              // overrides for updates that fix an advisory
    waitHours = 0
    windows = new Listing {}
    group = ""
  }
  lowestFix = true     // propose the lowest full fix past permitted and channel
  images = true        // compare pinned images' old and new SBOMs
  imagePlatforms = new Listing { "linux/amd64" }   // add "linux/arm64" to compare it too
  quietImages = new Mapping {   // image pattern = why its warning is left off PRs
    ["ghcr.io/acme/internal-*"] = "built from our own Wolfi base, scanned in CI"
  }
}
```

`advisories = false` turns the lookup off for a repository, images included, and
`images = false` turns off only the image comparison. With `images` on,
`imagePlatforms` must list at least one platform. An image in
`quietImages`, matched like a dependency pattern, gets no warning on its PRs;
the dashboard still lists it, under "Silenced", with your reason.
