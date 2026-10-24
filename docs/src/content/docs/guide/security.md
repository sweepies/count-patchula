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
installed. Neither are floating action tags like `@v4`, container images,
mise tools or Terraform providers, which OSV does not cover.

Each run sends one batched query to OSV with the package names and versions,
and nothing else from your repositories. Advisory details are kept in the cache
and fetched again only when OSV reports them modified, so a warm run costs one
request. If OSV cannot be reached, the run carries on without advisories and
the report says why in a warning.

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

## Configuration

```pkl
security {
  advisories = true    // check dependencies against OSV.dev
  label = "security"   // added to PRs that fix an advisory; "" adds none
}
```

`advisories = false` turns the lookup off for a repository.
