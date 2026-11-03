---
title: "Sources"
description: "Where Count Patchula looks up versions: npm, crates.io, the Go proxy, PyPI, container registries, Helm, Terraform, Maven and more."
---
A source is where versions come from. Each dependency has one, chosen by its
manager, and rules can select by it.

| Source | Package identity | Where versions come from |
|---|---|---|
| `npm` | package name | the npm registry, full metadata for publication times, dist-tags and deprecations |
| `crates` | crate name | the crates.io sparse index, with publication times; yanked releases are skipped |
| `go` | module path | the Go module proxy (`@v/list`, `.info` for new versions only) |
| `pypi` | normalised name | the PyPI JSON API, falling back to the PEP 691 simple API |
| `docker` | image | registry v2 tags and manifests on Docker Hub, GHCR and other registries |
| `oci` | `registry/chart` | OCI registry tags for Helm charts |
| `helm` | `https://repo\|chart` | the repository's `index.yaml` |
| `terraform` | `namespace/provider` or `namespace/module/system` | the public Terraform registry |
| `github-releases`, `github-tags` | `owner/repo` | GitHub, through mise-versions when it can |
| `github-runners` | runner family | a catalog of GitHub-hosted runner labels |
| `forge-tags` | `host/path` | GitLab and Gitea/Forgejo tags, for pre-commit hooks and GitLab CI includes and components |
| `mise` | tool name, as written in `mise.toml` | `mise ls-remote --json`, run by the Count's pinned mise ([mise lookups](#mise-lookups)) |
| `maven` | `group:artifact` | `maven-metadata.xml` on Maven Central, Google Maven or the Gradle plugin portal |
| `nuget` | package ID | the nuget.org flat container and registration pages |
| `rubygems` | gem name | the RubyGems versions API |
| `packagist` | `vendor/name` | Packagist's p2 metadata |

## Fetching less

Every response is stored with its validators, and every repeat request is
conditional. Unauthenticated registries honour their declared `max-age`. Go proxy
lists and validator-free OCI tag pages get a one-hour local freshness floor
(`http.goFreshnessSecs` and `http.registryFreshnessSecs` in the
[settings](/reference/settings/); `0` restores upstream behaviour), because those endpoints would otherwise be read on every run.

GitHub histories are read in pieces:

- A cold release history is fetched through batched GraphQL queries, sixteen
  sources at a time.
- A warm one starts with a conditional REST read of the newest releases. Only
  the fields that matter (tag, date, draft, prerelease) feed the fingerprint,
  so a download counter changing does not trigger a refetch.
- When the head did change, history is read newest first and merged into the
  cache until a known release turns up.
- Tags use one conditional read of the matching refs, then hydrate only new or
  moved tags.

A history-head check cannot see a release deleted or edited behind an
unchanged head. Webhook invalidation or periodic reconciliation covers that.

## mise lookups

A `mise` dependency's versions are what `mise ls-remote --json --prerelease
<tool>` prints, so every backend mise reads release metadata from works, and
the Count keeps no copy of mise's registry or backend rules. mise lists a
version, its date, whether it is a prerelease and, for GitHub-hosted tools, a
release URL. Backends leave out what they do not know: Node lists a date only,
and some `go:` versions have no date and so no release-age wait. Prereleases
are listed, and the Count's [prerelease rules](/guide/lookup-policy/) decide
whether to offer them; mise's `prerelease` flag does not filter.

The Count runs mise the way he runs the lockfile tools, and more strictly:

- With his own pinned mise from the [tool
  store](/guide/lockfiles/#the-tool-store), not yours.
- From a new, empty directory for each lookup, with a scratch `HOME` and
  nothing inherited from the Count's environment. mise loads no configuration
  (`MISE_NO_CONFIG`), env templates (`MISE_NO_ENV`) or hooks
  (`MISE_NO_HOOKS`), so neither your global configuration nor a repository's
  `mise.toml` can apply.
- With tool names validated first: no leading `-`, no bracketed options and no
  `@version` selector.
- With the backends `asdf` and `vfox` disabled (`MISE_DISABLE_BACKENDS`), and
  names for them refused before mise starts. A backend prefix mise does not
  know fails in mise, which runs no plugin for it. Nothing installs during a lookup
  (`MISE_AUTO_INSTALL=0`).
- With the GitHub token he runs with, as `MISE_GITHUB_TOKEN`, only when he
  talks to github.com, and no other credential: mise's `gh` login, credential
  command and Git credential helper lookups are off.
- With a three-minute limit per lookup and at most four running at once.

What happens to a listing:

- It is cached for an hour, per tool, token scope and mirror setting. A manual
  retry, or a failed lookup, asks mise again.
- The Node LTS line comes from `mise ls-remote node lts`. When that listing
  fails or is empty, the catalog stays whole without LTS evidence, so only a
  dependency on the LTS channel reports it.
- A tool installed from a Git ref (`cargo:` with a Git URL) lists only `HEAD`,
  which the Count reports as having no release versions.
- Any other listing is kept. Versions that do not order are never proposed, and
  a pin that does not order is reported as one the Count cannot classify.
- Where there is no release URL, release notes find a tool's repository from
  the backend mise picks for it when that is npm, PyPI or crates.io. A tool
  whose listing and backend name no repository has no release notes.

## mise-versions host

By default (`useVersionsHost = true`), public GitHub repositories that appear in
mise's registry are read from the public
[mise-versions](https://mise-versions.jdx.dev) mirror instead of the GitHub API:
`releases.json` and `tags.json`, validated by ETag. Repositories outside the
registry, including private ones, go straight to GitHub. If the mirror fails
with a transport error, 429 or 5xx, the Count falls back to GitHub; a 403, 404 or
empty list does not, and a truncated list is reported as an incomplete lookup.
mise tools do not use this path: mise reads the mirror itself, and
`useVersionsHost = false` turns that off too (`MISE_USE_VERSIONS_HOST=0`). Set
it to read everything upstream.

## Registries and limits

Docker Hub, GHCR and GitLab registries use anonymous registry tokens, cached
until shortly before they expire. A token comes from the registry's own host,
except Docker Hub's from `auth.docker.io` and a GitLab registry's
(`registry.gitlab.com`) from its GitLab instance (`gitlab.com/jwt/auth`).
Authenticated pulls, Docker Hub's included, use a [private
registry](#private-registries) entry. When Docker Hub's `ratelimit-remaining`
hits zero, queued reads stop. A 429 shares a cooldown across the host and
retries at most three times, honouring `Retry-After`. If the wait is too long,
the last cached response is used and marked stale in the report; a cold miss is
still an error.

Redirects are followed up to five times, HTTPS only, with credentials stripped
when the origin changes.

## Private registries

The operator [settings](/reference/settings/) hold credentials for private
registries, keyed by an https URL prefix. Repository configs never name
credentials: a `patchula.pkl` can point a dependency at any URL, so in the
server it could otherwise read another organisation's registry.

```pkl
amends "PatchulaSettings.pkl"

registries {
  ["https://acme.jfrog.io/artifactory/api/helm/charts"] {
    auth = "bearer"
    secret { env = "ARTIFACTORY_TOKEN" }
  }
  ["https://acme.jfrog.io"] {
    auth = "basic"
    username = "ci-bot"
    secret { file = "/run/secrets/artifactory" }
  }
  ["https://registry-1.docker.io"] {
    auth = "basic"
    username = "acme"
    secret { env = "DOCKERHUB_TOKEN" }
  }
}
```

- A request whose URL is under a prefix, matched by whole path segments on the
  same origin, carries that entry's credentials. The longest prefix wins.
- `bearer` sends the secret as a bearer token. `basic` sends the username and
  the secret to the URLs under the prefix, including a container registry's
  first request. When a registry with an entry answers with a token service
  instead, the Count exchanges credentials there for a pull token: the
  registry's own at a token service on its origin, Docker Hub's
  `auth.docker.io`, or `gitlab.com` for `registry.gitlab.com`, and otherwise
  only a token service's own entry. A self-hosted GitLab registry needs entries
  for both the registry and its GitLab's `/jwt/auth`. A registry without an
  entry only ever gets anonymous tokens. Tokens issued to an account stay in
  memory, and within the entry's prefix.
- A secret comes from an environment variable or a file, never the settings
  file. The server reads secrets again for every batch, so a rotated secret
  applies without a restart; one it cannot read is logged, and requests under
  that prefix carry no credentials, not even a broader entry's. The CLI stops
  with a usage error instead.
- Responses read with credentials are cached under their entry and a one-way
  digest of its credentials, so a read without that entry, or after the secret
  or account changes, never sees them.
- In the server, an entry's `allowedInstallations` limits which installations
  use it. For any other installation, requests under its prefix carry no
  credentials.
- A redirect carries credentials only to a URL an entry covers, and only from
  a request that sent some: a request without credentials never gains any on
  a redirect. A token issued under an entry stays within that entry's prefix.

A dependency's source decides where its versions come from. Helm repositories,
OCI and Docker registries, Maven repositories and GitLab, Gitea or Forgejo tags carry
their URL, so a private one works with an entry. npm and Python dependencies
can name their own [package registry](#package-registries). The other package
registries still read their public registry, or its mirror. Lockfile tools get
no registry credentials yet.

### Package registries

A dependency can come from a registry of its own instead of its source's
public one. The repository says which, and the report shows it as the
dependency's `registry`:

- npm reads `.npmrc` files at or above each `package.json`:
  `@scope:registry=` for a scope, else `registry=` for every package, each
  from the nearest file that sets it. Yarn Berry's `npmRegistryServer` in
  `.yarnrc.yml`, at the top level and under `npmScopes`, works the same and
  wins over `.npmrc` in the same directory. A file that names no registry
  does not hide one further up. Packages only a lockfile records get the same
  registry as a declared one would. `packageManager` stays on the public
  registry, since Corepack does not read `.npmrc`.
- Python packages come from the index the project pins them to, else the one
  their lockfile records, else the only index the project names:
  - uv: `[[tool.uv.index]]` (a `default` one replaces PyPI, an `explicit` one
    serves only packages pinned to it, and a `flat` one is a find-links
    source), `index-url`, `extra-index-url`, `find-links` and `no-index`,
    from the nearest `uv.toml`, or `pyproject.toml` with `[tool.uv]`, at or
    above the project. A workspace member takes them from its workspace root
    instead. Pins in `[tool.uv.sources]` come from the project and its
    workspace root, the root that lists it in `members` and not in `exclude`,
    and `no-sources` turns them off as it does for uv. An override or
    constraint that names a URL takes its package from there.
  - Poetry: `[[tool.poetry.source]]` (`primary` replaces PyPI, and more than
    one is ambiguous; `supplemental` adds an index; `explicit` serves only
    pinned packages; a source named PyPI without a URL is PyPI) and a
    dependency's `source`, matched without regard to case.
  - `uv.lock` and `poetry.lock` record each package's index. Only the
    project's own lockfile counts, or its uv workspace root's. A package whose
    entries disagree is ambiguous, and so is one locked to an index the
    project no longer searches: the tool would resolve it again.

  Lists may be written as `[[...]]` tables or as inline arrays alike.

  A package from git, a path, a URL or a workspace member, by pin or by
  lockfile, is not looked up at all: its name on PyPI may be someone else's.
  It is reported as skipped instead.

  A package that is pinned nowhere and unlocked, in a project that names an
  index besides PyPI, could come from either, which is how a public package
  can stand in for a private one. The Count does not guess: the package's
  lookup fails, saying it could come from several indexes. Pin it, or commit
  a lockfile. A pin that holds only under some markers, extras, groups or
  Pythons is ambiguous too, since elsewhere the package comes from the
  project's indexes.

  Everything else fails closed. Every setting uv accepts is either read or
  known not to move packages, and every key a Poetry source or dependency
  may have is known; tests check both lists against uv and Poetry
  themselves. A key the Count does not know, a section it cannot read, a
  `pyproject.toml` or `uv.toml` at or above the project that does not parse,
  a pin to an index the project does not define, or uv's `torch-backend`
  fails the lookups it could affect. So do index settings of tools the Count
  cannot check against the tool: PDM sources and overrides, Rye sources,
  Pixi PyPI options, and Hatch environments that set `PIP_*` or `UV_*`
  variables. Indexes come only from these files, never from `pip.conf`,
  `PIP_*` or `UV_*` variables, Dockerfiles or CI workflows. Private indexes
  are read through their simple API, as PEP 691 JSON or PEP 503 HTML; HTML
  gives no upload times, so no release-age wait.

  pip requirements files are not maintained at all; see
  [managers](/guide/managers/#python).
- An npm or PyPI [custom extractor](/guide/configuration/#custom-extractors) can set
  `registry`, a template such as `"{registry}"`; an empty or unmatched capture
  means the public registry. Without it, npm packages follow `.npmrc` like a
  `package.json` beside them.

`.npmrc` and `.yarnrc.yml` are read whenever npm, Bun or an npm extractor is
enabled, and every `pyproject.toml`, `uv.toml`, `uv.lock` and `poetry.lock`
whenever pyproject is. `files` and `exclude` do not limit these. Bun's
`bunfig.toml` and Yarn 1's `.yarnrc` are not read yet: a registry set only
there is not used.

npm's, Yarn's and PyPI's public registries (`registry.npmjs.org`,
`registry.yarnpkg.com`, `pypi.org`, over https or plain http) count as none. A value the Count cannot read from,
such as a `${VAR}` npm would take from the environment or a plain-http URL, is
recorded instead: that package's lookup fails, rather than reading the public
package of its name, which may be someone else's. Any user, password, query
or fragment is removed from it first, and a URL that does not parse cleanly
is recorded only as `<unreadable URL>`. A token in the path of a URL, as some
hosted registries issue, looks like any other path and is recorded; keep such
URLs in the operator's registry settings rather than the repository.

A repository naming a registry never brings credentials: requests to it carry
an operator entry's, when one covers its URL, and nothing otherwise. Two
packages with the same name from different registries are separate
dependencies, with separate lookups and branches. A package from its own
registry gets no [advisories](/guide/security/) and no release notes, since
the public package of that name is a different package.

### Mirrors

Where the public registries are out of reach, or should only be read through
a proxy such as an Artifactory remote repository or a Nexus proxy, an entry
can stand in for one with `mirrorOf`:

```pkl
registries {
  ["https://acme.jfrog.io/artifactory/api/npm/npm-remote"] {
    auth = "bearer"
    secret { env = "ARTIFACTORY_TOKEN" }
    mirrorOf = "https://registry.npmjs.org"
  }
  ["https://nexus.acme.dev/repository/pypi-proxy/simple"] {
    mirrorOf = "https://pypi.org/simple"
  }
}
```

- A request under the `mirrorOf` prefix goes to the entry's prefix instead,
  with the rest of the URL unchanged, and carries the entry's credentials if it
  has `auth`. An entry without `auth` sends none.
- There is no fallback: when the mirror fails, the lookup fails.
- Dependencies keep their public identity, so branches, rules and the
  dashboard do not change when a mirror is added or removed. Responses are
  cached under the mirror's entry.
- The mirror must serve the same paths as the registry it stands in for. A
  mirror that serves only part of an API, such as PyPI's simple index but not
  its JSON API, covers only the prefix it names; the rest is still read from
  the public registry.
- Container registries are not mirrored: their token services belong to the
  registry's own host.
- Release notes and advisories come from GitHub and OSV.dev, not the registry,
  and are not mirrored.

## Dates and release age

Publication dates feed the [release-age wait](/guide/lookup-policy/). Go dates
releases through the proxy's `.info` documents, only for the 20 newest on a cold
lookup, since a module can list hundreds and older releases are outside any
wait; versions the proxy lists but cannot describe are dropped. npm, crates,
PyPI, RubyGems, NuGet and Packagist report them directly. Maven Central's search
API lags by months, so Maven uses each POM's `Last-Modified` header, for the ten
newest versions only. GitHub tags are dated [as the release-age
wait describes](/guide/lookup-policy/#the-release-age-wait). mise tools use the
date mise lists, which is a backend's own release or publication date. GitLab
tags use the annotated tag's date. Lightweight GitLab tags, Gitea and Forgejo
tags, and hosted runner labels are undated and have no wait.

Where release notes look for each dependency's repository is under [finding
the upstream](/guide/release-notes/#finding-the-upstream).
