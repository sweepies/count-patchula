---
title: "Benchmarks"
description: "Count Patchula against Renovate on six large public repositories: lookup time cold and warm, and peak memory."
---
Count Patchula against Renovate 44.261.0 on six large public repositories,
looking up updates without writing anything. Each tool ran each repository
alone on the same 4 vCPU AMD EPYC 4465P VM (7.8 GiB), first cold, then warm.

| | Count Patchula | Renovate | |
|---|---|---|---|
| All six, cold | **42.6 s** | 1,351.2 s | 31.7× faster |
| All six, warm | **6.4 s** | 392.1 s | 61.7× faster |
| `grafana/grafana`, cold | **10.5 s** | 954.7 s | 90.8× faster |
| Peak memory, grafana cold | **257 MB** | 4.1 GB | 16.5× less |

## Per repository

Wall time and peak memory.

| Repository | Patchula cold | Patchula warm | Renovate cold | Renovate warm |
|---|---|---|---|---|
| mastodon/mastodon | 9.7 s, 195 MB | 1.0 s, 69 MB | 152.6 s, 2.0 GB | 134.2 s, 905 MB |
| getsentry/sentry | 8.3 s, 124 MB | 1.2 s, 67 MB | 104.5 s, 2.0 GB | 60.4 s, 1.1 GB |
| grafana/grafana | 10.5 s, 257 MB | 1.8 s, 163 MB | 954.7 s, 4.1 GB | 95.3 s, 2.1 GB |
| monicahq/monica | 3.4 s, 72 MB | 0.7 s, 42 MB | 44.6 s, 911 MB | 35.3 s, 637 MB |
| jellyfin/jellyfin | 5.5 s, 51 MB | 0.9 s, 38 MB | 30.9 s, 763 MB | 29.0 s, 689 MB |
| apache/kafka | 5.2 s, 56 MB | 0.8 s, 39 MB | 64.0 s, 1.2 GB | 37.9 s, 880 MB |

## Coverage

Unique dependency names found (Renovate's unskipped dependencies).

| Repository | Patchula | Renovate | Both |
|---|---|---|---|
| mastodon | 290 | 338 | 288 |
| sentry | 432 | 444 | 424 |
| grafana | 1,275 | 1,255 | 1,208 |
| monica | 87 | 84 | 81 |
| jellyfin | 108 | 108 | 106 |
| kafka | 83 | 128 | 71 |

Most of mastodon's gap is gems with no version requirement, which the Count
leaves to lockfile maintenance. Kafka's is versions in Groovy build scripts
outside the standard catalogs and dependency blocks.

## Notes

- Count Patchula ran on 2026-11-25 (`955ff81a`, medians across 3 cold/warm runs
  per repository, 5 cold runs for `mastodon/mastodon`; speedups and ratios are
  computed from unrounded medians and MiB values). Renovate 44.261.0 ran on
  2026-11-24 on the same VM in Docker with each repository's own config,
  `dryRun=lookup`, `repositoryCache=enabled` and `persistRepoData=true` so warm
  runs kept both the clone and cache.
- Renovate's cold time includes cloning (and container startup, ~85 s across
  all six; using Renovate's internal `durationMs` alone is 1,266.4 s cold /
  29.7× and 366.2 s warm / 57.6×). The Count reads files through the GitHub
  API, and lookups never clone.
- Peak memory is process Max RSS for the Count (lookups spawn no child
  processes) and cgroup v2 `memory.peak` for Renovate (which includes `git`
  and other child processes). Total CPU time across all six repositories was
  15.3 s cold / 1.0 s warm for the Count against 891.0 s cold / 333.8 s warm
  for Renovate.
