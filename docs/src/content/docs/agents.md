---
title: "For coding agents"
---
Count Patchula ships two files for coding agents. Both are fetched from the
repository, so they stay in step with `main`.

- [Agent setup](https://raw.githubusercontent.com/sweepies/count-patchula/main/docs/agent-setup.md)
  covers replacing Renovate, choosing how to run the Count, writing
  `patchula.pkl` and validating it. It marks every step that needs a decision
  from the user with **Ask**.
- [The patchula skill](https://raw.githubusercontent.com/sweepies/count-patchula/main/skills/patchula/SKILL.md)
  is the short reference an agent loads while working: configuring, running,
  reading reports and handling pull requests.

The mise install ships the skill with the release, so it needs no separate
download. See [installation](/installation/).

## Safety rules

- `patchula lookup` and `patchula run --dry-run` write nothing to GitHub. Use
  them before anything that opens branches or pull requests.
- Never ask for a token or an App private key in chat. Say where it goes and
  let the user set it.
- Validate every config change with `patchula config check .`. An invalid
  config exits 2 and names the file and field.
- An unknown field is an error, not a warning, so read the
  [configuration reference](/reference/configuration/) instead of guessing.
- A missing update usually has a reason: release age, a rule with
  `active = false`, a `permitted` filter, a PR limit or a schedule window. The
  [dashboard](/guide/dashboard/) lists each held update with its reason.

Read the [configuration guide](/guide/configuration/) for policy, the [JSON
report](/reference/report/) for field meanings, and [branches and
PRs](/guide/pull-requests/) for what the Count writes.
