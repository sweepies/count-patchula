<!-- patchula-dashboard:v1 -->

[The Count](https://count-patchula.sweepy.dev) keeps watch over this repository's dependencies. Tick a box and he will see to it on the next run.

- [ ] Re-check this repository <!-- patchula-action:{"action":"recheck","target":"TARGET"} -->

## Known vulnerabilities

> [!IMPORTANT]
> **16 known vulnerabilities** in `crypto-bigint`, `ctutils`, `rsa`, `unsafe-libyaml` and 1 more.

| Package | Version | Severity | Advisories | File | Status |
| --- | --- | --- | --- | --- | --- |
| **`crypto-bigint`** | `0.5.5` | — | [RUSTSEC-2026-0387](https://osv.dev/vulnerability/RUSTSEC-2026-0387) | `Cargo.lock` | Fixed in `0.7.6`, not proposed |
| **`ctutils`** | `0.4.3` | — | [RUSTSEC-2026-0385](https://osv.dev/vulnerability/RUSTSEC-2026-0385) | `Cargo.lock` | Fixed by #85 |
| **`rsa`** | `0.9.10` | — | [RUSTSEC-2023-0071](https://osv.dev/vulnerability/RUSTSEC-2023-0071) (`CVE-2023-49092`) | `crates/github/Cargo.toml`<br>`crates/server/Cargo.toml` | No fixed release yet |
| **`unsafe-libyaml`** | `0.2.11` | — | [RUSTSEC-2024-0449](https://osv.dev/vulnerability/RUSTSEC-2024-0449) | `Cargo.lock` | No fixed release yet |
| **`zstd-safe`** | `8.1.0` | — | 12 advisories ([RUSTSEC-2026-0395](https://osv.dev/vulnerability/RUSTSEC-2026-0395), [RUSTSEC-2026-0396](https://osv.dev/vulnerability/RUSTSEC-2026-0396), [RUSTSEC-2026-0397](https://osv.dev/vulnerability/RUSTSEC-2026-0397), [RUSTSEC-2026-0398](https://osv.dev/vulnerability/RUSTSEC-2026-0398), [RUSTSEC-2026-0399](https://osv.dev/vulnerability/RUSTSEC-2026-0399), [RUSTSEC-2026-0400](https://osv.dev/vulnerability/RUSTSEC-2026-0400), [RUSTSEC-2026-0401](https://osv.dev/vulnerability/RUSTSEC-2026-0401), [RUSTSEC-2026-0402](https://osv.dev/vulnerability/RUSTSEC-2026-0402), [RUSTSEC-2026-0403](https://osv.dev/vulnerability/RUSTSEC-2026-0403), [RUSTSEC-2026-0404](https://osv.dev/vulnerability/RUSTSEC-2026-0404), [RUSTSEC-2026-0405](https://osv.dev/vulnerability/RUSTSEC-2026-0405), [RUSTSEC-2026-0406](https://osv.dev/vulnerability/RUSTSEC-2026-0406)) | `Cargo.lock` | Fixed by #93 |

<details>
<summary><strong>Advisory details</strong> (16)</summary>

| Advisory | Package | Severity | Fixed&nbsp;in | Summary |
| --- | --- | --- | --- | --- |
| [RUSTSEC-2026-0387](https://osv.dev/vulnerability/RUSTSEC-2026-0387) | `crypto-bigint` | — | `0.7.6` | Multiple timing variability issues in operations intended to be constant-time |
| [RUSTSEC-2026-0385](https://osv.dev/vulnerability/RUSTSEC-2026-0385) | `ctutils` | — | `0.4.4` | Non-constant-time mask generation and &#96;u128&#96; comparisons |
| [RUSTSEC-2023-0071](https://osv.dev/vulnerability/RUSTSEC-2023-0071) (`CVE-2023-49092`) | `rsa` | — | — | Marvin Attack: potential key recovery through timing sidechannels |
| [RUSTSEC-2024-0449](https://osv.dev/vulnerability/RUSTSEC-2024-0449) | `unsafe-libyaml` | — | — | unsafe-libyaml is unmaintained |
| [RUSTSEC-2026-0395](https://osv.dev/vulnerability/RUSTSEC-2026-0395) | `zstd-safe` | — | `8.2.0` | Use-after-free with &#96;CParameter::StableInBuffer&#96; |
| [RUSTSEC-2026-0396](https://osv.dev/vulnerability/RUSTSEC-2026-0396) | `zstd-safe` | — | `8.2.0` | &#96;Seekable&#96; reads a NULL or out-of-bounds seek table |
| [RUSTSEC-2026-0397](https://osv.dev/vulnerability/RUSTSEC-2026-0397) | `zstd-safe` | — | `8.2.0` | &#96;OutBuffer::around&#95;pos&#96; exposes uninitialized memory |
| [RUSTSEC-2026-0398](https://osv.dev/vulnerability/RUSTSEC-2026-0398) | `zstd-safe` | — | `8.2.0` | &#96;Seekable::decompress&#96; sets a &#96;Vec&#96; length beyond its capacity for offsets past the end |
| [RUSTSEC-2026-0399](https://osv.dev/vulnerability/RUSTSEC-2026-0399) | `zstd-safe` | — | `8.2.0` | Use-after-free through &#96;init&#95;using&#95;cdict&#96; and &#96;init&#95;using&#95;ddict&#96; |
| [RUSTSEC-2026-0400](https://osv.dev/vulnerability/RUSTSEC-2026-0400) | `zstd-safe` | — | `8.2.0` | Use-after-free of a dictionary after a one-shot call on a context with an open stream |
| [RUSTSEC-2026-0401](https://osv.dev/vulnerability/RUSTSEC-2026-0401) | `zstd-safe` | — | `8.2.0` | Out-of-bounds read in &#96;train&#95;from&#95;buffer&#96; when sample sizes overflow |
| [RUSTSEC-2026-0402](https://osv.dev/vulnerability/RUSTSEC-2026-0402) | `zstd-safe` | — | `8.2.0` | Heap buffer overflow in &#96;CCtx::try&#95;clone&#96; on a multithreaded context |
| [RUSTSEC-2026-0403](https://osv.dev/vulnerability/RUSTSEC-2026-0403) | `zstd-safe` | — | `8.2.0` | Uninitialized memory exposed when streaming decompression of a legacy frame fails |
| [RUSTSEC-2026-0404](https://osv.dev/vulnerability/RUSTSEC-2026-0404) | `zstd-safe` | — | `8.2.0` | Double free in &#96;DCtx::try&#95;clone&#96; |
| [RUSTSEC-2026-0405](https://osv.dev/vulnerability/RUSTSEC-2026-0405) | `zstd-safe` | — | `8.2.0` | Out-of-bounds read with an out-of-range &#96;InBuffer::pos&#96; |
| [RUSTSEC-2026-0406](https://osv.dev/vulnerability/RUSTSEC-2026-0406) | `zstd-safe` | — | `8.2.0` | Use-after-free when changing a multithreaded context with an unfinished frame |

</details>

## Open PRs

<sub>Tick an open PR to add it to the batch PR.</sub>

- [ ] #85 chore(deps): update `ctutils` to 0.4.4 &#91;security&#93; <kbd>patch</kbd> <!-- patchula-action:{"action":"batch","target":"TARGET"} -->
- [ ] #93 chore(deps): update `zstd-safe` to 8.2.0 &#91;security&#93; <kbd>patch</kbd> <!-- patchula-action:{"action":"batch","target":"TARGET"} -->
- [ ] #103 chore(deps): update `@astrojs/starlight` to 0.42.7 <kbd>patch</kbd> <!-- patchula-action:{"action":"batch","target":"TARGET"} -->

## Pending updates

<details>
<summary><strong>Waiting for their release age</strong> (1)</summary>

<sub>Too fresh for the configured release age, each until the time shown. Tick one and the Count creates it now anyway.</sub>

- [ ] **`@astrojs/starlight`** `0.42.6`&nbsp;→&nbsp;`0.42.8` <kbd>patch</kbd> · `docs/package.json` · <sub>eligible 31 Oct 08:29 UTC</sub> <!-- patchula-action:{"action":"create","target":"TARGET"} -->

</details>

<details>
<summary><strong>Held for other reasons</strong> (2)</summary>

<sub>No box here: each says what holds it.</sub>

- chore(deps): update `base64` to 0.23.1: lockfile generation failed: cargo: exit status: 101
  <details>
  <summary><strong>The tool's output</strong></summary>

  ```text
  error: failed to select a version for the requirement `base64 = "^0.22"`
  candidate versions found which didn't match: 0.23.1
  location searched: crates.io index
  required by package `jsonwebtoken v10.4.0`
  ... which satisfies dependency `jsonwebtoken = "^10"` (locked to 10.4.0) of package `patchula-github v0.6.0 (/home/runner/work/_temp/patchula/worktrees/run-SdTzSl/tree/crates/github)`
  ... which satisfies path dependency `patchula-github` (locked to 0.6.0) of package `patchula-engine v0.6.0 (/home/runner/work/_temp/patchula/worktrees/run-SdTzSl/tree/crates/engine)`
  ... which satisfies path dependency `patchula-engine` (locked to 0.6.0) of package `patchula-cli v0.6.0 (/home/runner/work/_temp/patchula/worktrees/run-SdTzSl/tree/crates/cli)`
  ```

  </details>
- chore(deps): update `node` to 26.13.0: lockfile generation failed: mise lock did not resolve node to 26.13.0

</details>

## Lookup warnings and errors

<sub>Tick a package to retry its lookup.</sub>

- [ ] **`patchula-cache`** (<kbd>crates</kbd> · `crates/cli/Cargo.toml`, `crates/datasource/Cargo.toml`, `crates/http/Cargo.toml`) <!-- patchula-action:{"action":"retry","target":"TARGET"} -->
  - Lookup failed (crates, not-found): HTTP 404

<details>
<summary><strong>Every dependency found</strong> (10 in 7 files)</summary>

<details>
<summary><code>Cargo.lock</code> (<kbd>cargo</kbd>, 4 dependencies)</summary>

| Dependency | Current |
| --- | --- |
| `crypto-bigint` | `0.5.5` |
| `ctutils` | `0.4.3` |
| `unsafe-libyaml` | `0.2.11` |
| `zstd-safe` | `8.1.0` |

</details>

<details>
<summary><code>crates/cli/Cargo.toml</code> (<kbd>cargo</kbd>, 1 dependency)</summary>

| Dependency | Current |
| --- | --- |
| `patchula-cache` | `0.6.0` |

</details>

<details>
<summary><code>crates/datasource/Cargo.toml</code> (<kbd>cargo</kbd>, 1 dependency)</summary>

| Dependency | Current |
| --- | --- |
| `patchula-cache` | `0.6.0` |

</details>

<details>
<summary><code>crates/github/Cargo.toml</code> (<kbd>cargo</kbd>, 1 dependency)</summary>

| Dependency | Current |
| --- | --- |
| `rsa` | `0.9.10` |

</details>

<details>
<summary><code>crates/http/Cargo.toml</code> (<kbd>cargo</kbd>, 1 dependency)</summary>

| Dependency | Current |
| --- | --- |
| `patchula-cache` | `0.6.0` |

</details>

<details>
<summary><code>crates/server/Cargo.toml</code> (<kbd>cargo</kbd>, 1 dependency)</summary>

| Dependency | Current |
| --- | --- |
| `rsa` | `0.9.10` |

</details>

<details>
<summary><code>docs/package.json</code> (<kbd>npm</kbd>, 1 dependency)</summary>

| Dependency | Current |
| --- | --- |
| `@astrojs/starlight` | `0.42.6` |

</details>

</details>

---

<p>
<a href="https://count-patchula.sweepy.dev"><img align="left" src="https://raw.githubusercontent.com/sweepies/count-patchula/v{version}/assets/logo/bat.svg" width="52" alt="Count Patchula"></a>
<strong><a href="https://count-patchula.sweepy.dev">Count Patchula</a></strong> keeps watch over this repository<br>
<sub>The Count works for free, but if you find him helpful, <a href="https://github.com/sponsors/sweepies">consider supporting his development</a>.</sub><br clear="left">
</p>
