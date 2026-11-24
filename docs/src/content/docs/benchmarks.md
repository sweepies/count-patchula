---
title: "Benchmarks"
description: "Count Patchula against Renovate on six large public repositories: lookup time cold and warm, and peak memory."
---
Count Patchula against Renovate 44.261.0 on six large public repositories,
looking up updates without writing anything. Each tool ran each repository
alone on the same 4 vCPU AMD EPYC 4465P VM (7.8 GiB), first cold, then warm.

| | Count Patchula | Renovate | |
|---|---|---|---|
| All six, cold | **49.3 s** | 1,351.2 s | 27.4× faster |
| All six, warm | **11.9 s** | 392.1 s | 33.0× faster |
| `grafana/grafana`, cold | **13.3 s** | 954.7 s | 71.9× faster |
| Peak memory, grafana cold | **247 MB** | 4.1 GB | 17.2× less |

## Per repository

Wall time and peak memory.

| Repository | Patchula cold | Patchula warm | Renovate cold | Renovate warm |
|---|---|---|---|---|
| mastodon/mastodon | 9.5 s, 190 MB | 2.1 s, 67 MB | 152.6 s, 2.0 GB | 134.2 s, 905 MB |
| getsentry/sentry | 9.9 s, 123 MB | 1.8 s, 65 MB | 104.5 s, 2.0 GB | 60.4 s, 1.1 GB |
| grafana/grafana | 13.3 s, 247 MB | 3.1 s, 158 MB | 954.7 s, 4.1 GB | 95.3 s, 2.1 GB |
| monicahq/monica | 5.3 s, 73 MB | 2.4 s, 39 MB | 44.6 s, 911 MB | 35.3 s, 637 MB |
| jellyfin/jellyfin | 5.7 s, 47 MB | 1.2 s, 36 MB | 30.9 s, 763 MB | 29.0 s, 689 MB |
| apache/kafka | 5.6 s, 56 MB | 1.3 s, 37 MB | 64.0 s, 1.2 GB | 37.9 s, 880 MB |

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

- Run on 2026-11-24 (`46658138`). Renovate 44.261.0 ran in Docker with each
  repository's own config, `dryRun=lookup`, `repositoryCache=enabled` and
  `persistRepoData=true` so warm runs kept both the clone and cache.
- Renovate's cold time includes cloning (and container startup, ~85 s across
  all six; using Renovate's internal `durationMs` alone is 1,266.4 s cold /
  25.7× and 366.2 s warm / 30.8×). The Count reads files through the GitHub
  API, and lookups never clone.
- Peak memory is process Max RSS for the Count (lookups spawn no child
  processes) and cgroup v2 `memory.peak` for Renovate (which includes `git`
  and other child processes). Total CPU time across all six repositories was
  13.9 s cold / 0.9 s warm for the Count against 891.0 s cold / 333.8 s warm
  for Renovate.
