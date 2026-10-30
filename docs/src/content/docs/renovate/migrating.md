---
title: "Migrating a config"
---
There is no converter. A `renovate.json` is usually a handful of presets and a
few rules, and most of those presets turn out to be the Count's defaults
already. Translating by hand, or asking an agent to, takes minutes and leaves you
with a config you understand.

## Start from the defaults

`config:recommended`, `:semanticCommits`, `:dependencyDashboard`, `:pinAllExceptPeerDependencies`,
`helpers:pinGitHubActionDigests` and `docker:pinDigests` mostly describe what
the Count does without being asked: exact pins (ranges kept where a lockfile holds the version), digest pins for actions and
images, semantic commit prefixes and one PR per update. Turn the dashboard on
and you are close.

```pkl
// patchula.pkl
amends "PatchulaConfig.pkl"

prs { dashboard = true }
```

Read through the [defaults](/guide/configuration/#defaults) and keep what you
actually rely on. [Departures](/renovate/departures/) lists the defaults that
differ from Renovate's, such as the 24-hour release-age wait and exact npm pins.

## Option by option

| Renovate | Count Patchula |
|---|---|
| `extends` | `amends "org.pkl"` or `import` |
| `enabledManagers` | `managers` (Renovate names differ: `gomod` is `go`, `docker-compose` is `compose`, `pip_requirements` is `requirements`, `pep621` and `poetry` are `pyproject`, `helmv3` is `helm`) |
| `ignoreDeps` | `ignore`, with globs and regexes |
| `ignorePaths` | `exclude` |
| `managerFilePatterns` | `files` |
| `packageRules` | `rules` |
| `matchPackageNames` | `select { packages { ... } }` |
| `matchDepNames` | `select { dependencies { ... } }` |
| `matchManagers`, `matchDatasources` | `select { managers }`, `select { sources }` |
| `matchFileNames` | `select { files }` |
| `matchUpdateTypes` | `select { updates }` (`pinDigest` is `pin`) |
| `matchDepTypes` | `select { kinds }` |
| `matchCurrentVersion` | `select { current = "semver:..." }` |
| `allowedVersions` | `apply { permitted = "semver:..." }` |
| `enabled: false` | `apply { active = false }` |
| `groupName` | `apply { group = "..." }` |
| `automerge` | `apply { merge = true }` |
| `rangeStrategy` | `apply { versions = "exact" \| "preserve" \| "widen" }` |
| `pinDigests: false` | `apply { digest = false }` |
| `minimumReleaseAge` | `apply { waitHours = 72 }` |
| `schedule`, `timezone` | `windows { new Window { cron { ... } zone = "..." } }` |
| `labels`, `addLabels` | `labels` |
| `semanticCommitType`, `commitMessagePrefix` | `commitPrefix` |
| `prBodyNotes` | `bodyNotes` |
| `fetchChangeLogs: "off"` | `releaseNotes = false` |
| `prConcurrentLimit`, `prHourlyLimit` | `prs { concurrent; perHour }` |
| `branchPrefix` | `prs { branch }` |
| `rebaseWhen` | `prs { rebase = "conflicts" \| "behind" }` |
| `dependencyDashboard` | `prs { dashboard = true }` |
| `lockFileMaintenance` | `lockfiles { refresh = true }` |
| `postUpdateOptions: ["gomodTidy"]` | `lockfiles { tidyGo = true }` |
| `customManagers` (regex) | `extractors` |
| `regexManagers` capture `depName` | capture `dependency` |
| `currentValue` | `current` |
| `extractVersion` | `extract` |
| `datasourceTemplate`, `depNameTemplate`, `packageNameTemplate`, `versioningTemplate` | `source`, `dependency`, `package`, `scheme` |

Watch for these when translating (the [configuration guide](/guide/configuration/)
has the details):

- A later rule replaces an earlier one's lists instead of merging them.
- Empty `group` means separate PRs.
- Zero is never "unlimited"; use `null`.
- `concurrent` is 10 by default, so the first run on a neglected repository
  opens ten PRs and lists the rest on the dashboard.

## A worked example

```json title="renovate.json"
{
  "extends": ["config:recommended"],
  "schedule": ["before 6am on monday"],
  "packageRules": [
    {
      "matchUpdateTypes": ["minor", "patch"],
      "matchCurrentVersion": "!/^0/",
      "automerge": true
    },
    {
      "matchPackageNames": ["/^@vitest//", "vitest"],
      "groupName": "vitest"
    }
  ]
}
```

becomes

```pkl title="patchula.pkl"
amends "PatchulaConfig.pkl"

defaults {
  windows {
    new Window { cron { "* 0-5 * * 1" } }
  }
}
rules {
  new Rule {
    select {
      updates { "minor"; "patch" }
      current = "!semver:<1"
    }
    apply { merge = true }
  }
  new Rule {
    select { packages { "vitest"; "glob:@vitest/*" } }
    apply { group = "vitest" }
  }
}
```

Then check it:

```sh
patchula config check .
patchula run --dry-run owner/repo
```

The [examples](https://github.com/sweepies/count-patchula/tree/main/examples)
directory has a dozen real configs translated this way.

## Things that do not carry over

Presets, onboarding, config migration, `hostRules`, private registries,
`postUpgradeTasks`, replacement PRs and platforms other than GitHub have no
equivalent. `vulnerabilityAlerts` reads Dependabot alerts; the Count checks
[OSV.dev advisories](/guide/security/) instead, on by default. The
[compatibility matrix](/renovate/compatibility/) lists everything. A `# renovate:` comment used
by a regex manager can stay in your files: it is only text, and your Pkl
extractor can match it.
