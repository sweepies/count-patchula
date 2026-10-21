# Benchmarks

Count Patchula against Renovate 44.173.1 on six large public repositories,
looking up updates without writing anything. Each tool ran each repository
alone on the same c7i.xlarge (4 vCPU, 8 GiB), first cold, then warm.

| | Count Patchula | Renovate | |
|---|---|---|---|
| All six, cold | **150.5 s** | 920.6 s | 6.1× faster |
| All six, warm | **14.8 s** | 274.3 s | 18.5× faster |
| Peak memory, grafana cold | **494 MB** | 3.6 GB | 7.3× less |

## Per repository

Wall time and peak memory.

| Repository | Patchula cold | Patchula warm | Renovate cold | Renovate warm |
|---|---|---|---|---|
| mastodon/mastodon | 16.6 s, 383 MB | 2.0 s, 100 MB | 98.1 s, 1.8 GB | 96.4 s, 1.1 GB |
| getsentry/sentry | 8.3 s, 186 MB | 3.6 s, 97 MB | 53.7 s, 1.8 GB | 44.9 s, 1.6 GB |
| grafana/grafana | 109.7 s, 494 MB | 5.5 s, 212 MB | 706.9 s, 3.6 GB | 89.4 s, 2.2 GB |
| monicahq/monica | 7.5 s, 195 MB | 1.1 s, 45 MB | 16.8 s, 978 MB | 9.0 s, 568 MB |
| jellyfin/jellyfin | 4.9 s, 73 MB | 1.3 s, 54 MB | 14.1 s, 823 MB | 11.7 s, 747 MB |
| apache/kafka | 3.5 s, 57 MB | 1.3 s, 21 MB | 31.0 s, 905 MB | 22.9 s, 754 MB |

## Coverage

Unique dependency names found.

| Repository | Patchula | Renovate | Both |
|---|---|---|---|
| mastodon | 294 | 342 | 292 |
| sentry | 439 | 438 | 433 |
| grafana | 1,369 | 1,286 | 1,280 |
| monica | 121 | 114 | 114 |
| jellyfin | 108 | 108 | 106 |
| kafka | 46 | 128 | 35 |

Most of mastodon's gap is gems with no version requirement, which the Count
leaves to lockfile maintenance. Kafka's was versions in a Groovy map, which the
Gradle manager has read since these runs.

## Notes

- Run on 2026-10-21 with Count Patchula `e89e99c`. Renovate used each
  repository's own config with `dryRun=lookup`.
- Renovate's cold time includes cloning. The Count reads files through the
  GitHub API and never clones.
- Go lookups got faster after these runs, so grafana's cold time (1,076 Go
  modules) is now lower than shown.
