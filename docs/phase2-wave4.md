# Dependency dashboard (SWE-108)

`prs.dashboard = true` maintains one open issue titled **Dependency Dashboard**
initially. Discovery uses the exact `<!-- rustovate-dashboard:v1 -->` line and
the authenticated bot actor, so owners may rename it. Ambiguous owned dashboards
fail with a diagnostic rather than choosing one. Disabling the dashboard closes
the marked issue; enabling it again creates a new open issue.

The dashboard groups open PRs by update type, shows pending release-age targets
with eligibility times, PR budget and schedule holds, owner-closed targets,
dependency/source lookup warnings and stale provenance, and collapsible detected
dependencies by manager/file. Entries are sorted, escaped and bounded to GitHub's
body size. The receipt stores the rendered content hash; the fresh inventory
also verifies the actual body, so ticks and external edits cannot hide behind
the cached hash. Unchanged runs issue no dashboard mutation.

The shared GraphQL inventory includes open issue bodies, branch heads, open PR
bodies and closed PR receipt nodes. Both issue and ref pagination are followed
without treating a partial inventory as complete. Ordinary steady repositories
fit in one query. Issues from other authors and PRs without valid ownership
markers are excluded from the dashboard's ownership decisions.

Offline coverage uses synthetic GitHub transports, a checked-in rendering
snapshot, receipt restoration and an engine run with a reopened SQLite cache.
The warm engine test asserts one inventory query and zero mutations with the
dashboard enabled, for both open and owner-closed updates.
