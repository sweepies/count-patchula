# Deliberate departures from Renovate

rustovate is not a Renovate clone. Each entry records a behaviour that differs
on purpose, and why. The comparison harness links differences it finds to
entries here; a difference with no entry is treated as a bug.

| # | Area | Renovate | rustovate | Why |
|---|---|---|---|---|
| 1 | Config | JSON/JSON5 with presets, migrations | Pkl only, typed schema, unknown fields are errors | Typed, composable config with real diagnostics |
| 2 | Onboarding | Opens an onboarding PR in repos without config | Repos without config are skipped | Configs are added deliberately |
| 3 | Execution | One repository at a time, caches reset per repository | One process per batch, concurrent repos, batch-wide lookups | Speed |
| 4 | Release history | Fetches full release and tag lists | Incremental, newest-first, stops at the first known item | Speed and rate limits |
| 5 | Repository access | Clones each repository | Reads files through the API; clones only for lockfile tools | Speed |
| 6 | Caller interface | Log lines | Versioned JSON report | Logs are not an API |
