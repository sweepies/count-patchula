# Phase 2, wave 1 evidence

## SWE-104: intermittent local test failure

The original failure did not reproduce. At unmodified `09d8172`, an isolated
checkout passed 36 full offline workspace test runs (12 each with 1, 2 and 16
test threads), 50 focused engine replay runs, and a full `mise run ci`, including
lint and build. The original aggregate of 114 passed / 1 failed points to
`rustovate-engine::public_multi_repo_report_cold_and_warm`: the baseline reaches
114 passes immediately before that one-test integration binary. This identifies
a suspect, not a confirmed root cause. No speculative behavior fix is claimed.

Following the owner's fallback instruction, CI now retains each stage's complete
stdout/stderr in a unique directory under `/tmp` (override with
`RUSTOVATE_CI_LOG_DIR`). A failure prints the stage, failing test names and log
path on a `test result:` line, surviving the filter that lost the original
evidence. `mise run test` uses `--no-fail-fast`. The suspected replay logs its
evaluation instant and cold/warm counters before comparing results.

`mise run test:stress` repeats the suite with varied thread counts and keeps full
logs outside the checkout. `RUSTOVATE_STRESS_RUNS`, `RUSTOVATE_STRESS_LOG_DIR` and
`RUSTOVATE_STRESS_FILTER` bound and focus reproduction. Recording is disabled.
Tests use isolated caches and no shared environment mutations, ports or fixed
temporary directories. A recurrence should be investigated using its retained
log rather than inferred from an aggregate count.
