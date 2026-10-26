# Set up Count Patchula

These instructions are for a coding agent working in a user's repository.
Follow them in order to set up Count Patchula, a dependency update bot for
GitHub, and to replace Renovate if the repository uses it. Talk to the user at
each step marked **Ask**, and wait for the answer before going on.

## Throughout

- Never run `patchula run` without `--dry-run` until the user has confirmed
  that they want real branches and pull requests. `patchula lookup` and
  `patchula run --dry-run` write nothing to GitHub.
- Never ask the user to paste a secret, such as a GitHub token or a GitHub App
  private key, into the chat. Tell them where it goes (an environment variable,
  a repository secret, or the host's variables) and let them put it there.
- If the user does not use mise, offer it, as described below.

### Offer mise

Check whether the user already uses [mise](https://mise.jdx.dev). If not, offer
it before installing anything, and explain what it gives them:

- The recommended install is `mise use -g --minimum-release-age 0 packslip:sweepies/count-patchula`.
  mise checks the download against the release's signed manifest, and the
  manifest also ships the patchula skill for coding agents. See
  [installation](https://count-patchula.sweepy.dev/installation/).
- The Count regenerates lockfiles with the real tools, and runs them through
  mise, so mise must be on the `PATH` wherever the Count runs. The tools go
  into the Count's own tool store, not the user's mise installs. See
  [what the runner needs](https://count-patchula.sweepy.dev/installation/#what-the-runner-needs).
- A repository's own pins in `mise.toml` or `.tool-versions` decide which tool
  version regenerates its lockfiles. See
  [which tool version](https://count-patchula.sweepy.dev/guide/lockfiles/#which-tool-version).

Some setups do not need mise on the user's machine: the GitHub Action installs
the Count with mise itself, and a CLI that hands its work to a server with
`--remote` needs no mise and no GitHub token. A machine that runs the Count
itself, such as the server's host, still needs mise.

If the user declines, respect that and use one of the other methods on the
[installation](https://count-patchula.sweepy.dev/installation/) page: release archives, Cargo, or building from
a checkout. Tell them that lockfile regeneration still needs mise on the `PATH`
of the machine that runs the Count.

## 1. Learn the Count

Fetch the [patchula skill](https://raw.githubusercontent.com/sweepies/count-patchula/main/skills/patchula/SKILL.md) and read it. It
covers what Count Patchula is, how a repository is configured with
`patchula.pkl`, how to validate a config and how to run the CLI. Link the user
to pages of [the documentation](https://count-patchula.sweepy.dev/) rather than restating them at length.

## 2. Look at the current setup

Find out whether the repository already uses Renovate. Look for:

- `renovate.json`, `renovate.json5`, `.renovaterc`, `.renovaterc.json` or
  `.renovaterc.json5` at the root;
- `.github/renovate.json`, `.github/renovate.json5`, `.gitlab/renovate.json`
  or `.gitlab/renovate.json5`;
- a `renovate` key in `package.json`;
- workflows in `.github/workflows` that run self-hosted Renovate, such as
  `renovatebot/github-action` or a `renovate` container or npm package;
- Renovate-specific comments in files, such as `# renovate:` markers used by
  regex managers, and presets named in `extends`.

Also note `.github/dependabot.yml` if it exists. Dependabot and the Count would
propose the same updates.

If Renovate is in use, work out what the user relies on it for: the managers it
picks up, the presets in `extends`, `packageRules`, schedules, automerge,
grouping, custom managers and anything else in the config. Check each against:

- the [compatibility matrix](https://count-patchula.sweepy.dev/renovate/compatibility/), for platforms,
  managers, datasources, versioning, lockfiles and features;
- [deliberate departures](https://count-patchula.sweepy.dev/renovate/departures/), for behaviour that differs on
  purpose;
- the [migration guide](https://count-patchula.sweepy.dev/renovate/migrating/), for the option-by-option
  translation and what does not carry over.

Then tell the user plainly, before changing anything: what carries over, what
changes (for example, presets become explicit defaults, and repositories
without a config are skipped instead of getting an onboarding pull request),
and anything they use that the Count does not support. If something they rely
on is unsupported, **ask** whether to go on.

If the repository has Dependabot, **ask** whether to keep it or remove it once
the Count is set up.

## 3. Choose how to run it

**Ask** how the user wants to run the Count:

- **Locally, with the CLI.** The user runs `patchula` on their own machine. See
  [getting started](https://count-patchula.sweepy.dev/getting-started/).
- **In CI, with the GitHub Action.** A scheduled workflow runs the Count. It is
  the simplest setup for a handful of repositories. See
  [GitHub Actions](https://count-patchula.sweepy.dev/guide/github-actions/).
- **As a server, with the GitHub App server.** Recommend this one, and say
  why: caches stay warm between batches, so every batch after the first is
  cheap; webhooks batch together work from many repositories; and a ticked
  checkbox on a pull request or the dashboard is acted on within seconds
  instead of at the next scheduled run. See the
  [GitHub App server](https://count-patchula.sweepy.dev/guide/server/).

If the user chooses the server, **ask** where they want to deploy it. The docs
recommend [Railway](https://count-patchula.sweepy.dev/guide/server/#hosting-on-railway): it bills for the CPU
and memory a service actually uses, by the minute, and the server sits idle
between batches with little memory in use. Railway's Serverless setting must
stay off: a server that slept would miss its scheduled sweeps. The guide also
has a [systemd unit](https://count-patchula.sweepy.dev/guide/server/#systemd) for a machine of their own.

Take facts about each option from the linked pages, and give the user the
links.

## 4. Set it up

Set the Count up the way the user chose, installing it as agreed above.

1. Write `patchula.pkl` at the repository root. Start from the defaults in the
   [configuration guide](https://count-patchula.sweepy.dev/guide/configuration/) and add only what the user
   needs. If Renovate was in use, translate its config with the
   [migration guide](https://count-patchula.sweepy.dev/renovate/migrating/); many presets are already the
   Count's defaults. The [configuration reference](https://count-patchula.sweepy.dev/reference/configuration/)
   lists every field, and unknown fields are errors.
2. Validate the config, and fix it until this passes:

   ```sh
   patchula config check .
   ```

3. Follow the guide for the chosen way to run it:
   - CLI: the remaining steps of [getting started](https://count-patchula.sweepy.dev/getting-started/). The
     user exports a token in `PATCHULA_TOKEN` themselves; real runs need a
     GitHub App installation token.
   - GitHub Action: [GitHub Actions](https://count-patchula.sweepy.dev/guide/github-actions/). The user creates
     the GitHub App, stores its private key in the repository secret
     `PATCHULA_APP_PRIVATE_KEY` and its client ID in the repository variable
     `PATCHULA_CLIENT_ID`. Write the workflow file.
   - Server: [GitHub App server](https://count-patchula.sweepy.dev/guide/server/). The user creates the GitHub
     App and puts `PATCHULA_APP_ID`, `PATCHULA_APP_PRIVATE_KEY` and
     `PATCHULA_WEBHOOK_SECRET` in the host's variables, or the key and secret
     in files named by `PATCHULA_APP_PRIVATE_KEY_FILE` and
     `PATCHULA_WEBHOOK_SECRET_FILE`. Help with the server settings file and the
     deployment.
4. Show the user what the Count would do. With a token in the user's
   environment, or a server and `--remote`, run these in the checkout:

   ```sh
   patchula lookup
   patchula run --dry-run
   ```

   Go no further than `--dry-run` unless the user asks for a real run.
5. If Renovate was in use, remove its config files and any workflow that runs
   it, and drop the `renovate` key from `package.json`. Keep `# renovate:`
   comments that a Pkl extractor in `patchula.pkl` now matches. If the user has
   the Renovate GitHub App installed, tell them to uninstall it themselves, from
   the repository's or organisation's GitHub settings; it is not something you
   can do. Remove Dependabot's config only if the user asked to.

## 5. Land the change

**Ask** whether to commit the changes directly or open a pull request, then do
that. Summarise for the user what was added and removed, anything they still
have to do themselves (creating the GitHub App, storing secrets, uninstalling
Renovate's App), and how to start the first real run.
