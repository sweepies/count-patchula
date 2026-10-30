<!-- patchula-dashboard:v1 -->

The Count keeps watch over this repository's dependencies. Tick a box and he will see to it on the next run.

- [ ] Ask the Count to re-check this repository <!-- patchula-action:{"action":"recheck","target":"TARGET"} -->

## Known vulnerabilities

- `crypto-bigint` 0.5.5 in `Cargo.lock`: [RUSTSEC-2026-0387](https://osv.dev/vulnerability/RUSTSEC-2026-0387): Multiple timing variability issues in operations intended to be constant-time. Fixed in `0.7.6`, not proposed.
- `ctutils` 0.4.3 in `Cargo.lock`: [RUSTSEC-2026-0385](https://osv.dev/vulnerability/RUSTSEC-2026-0385): Non-constant-time mask generation and &#96;u128&#96; comparisons. Fixed by #85.
- `rsa` 0.9.10 in `crates/github/Cargo.toml`, `crates/server/Cargo.toml`: [RUSTSEC-2023-0071](https://osv.dev/vulnerability/RUSTSEC-2023-0071) (CVE-2023-49092): Marvin Attack: potential key recovery through timing sidechannels. No fixed release yet.
- `unsafe-libyaml` 0.2.11 in `Cargo.lock`: [RUSTSEC-2024-0449](https://osv.dev/vulnerability/RUSTSEC-2024-0449): unsafe-libyaml is unmaintained. No fixed release yet.
- `zstd-safe` 8.1.0 in `Cargo.lock`: 12 advisories ([RUSTSEC-2026-0395](https://osv.dev/vulnerability/RUSTSEC-2026-0395), [RUSTSEC-2026-0396](https://osv.dev/vulnerability/RUSTSEC-2026-0396), [RUSTSEC-2026-0397](https://osv.dev/vulnerability/RUSTSEC-2026-0397), [RUSTSEC-2026-0398](https://osv.dev/vulnerability/RUSTSEC-2026-0398), [RUSTSEC-2026-0399](https://osv.dev/vulnerability/RUSTSEC-2026-0399), [RUSTSEC-2026-0400](https://osv.dev/vulnerability/RUSTSEC-2026-0400), [RUSTSEC-2026-0401](https://osv.dev/vulnerability/RUSTSEC-2026-0401), [RUSTSEC-2026-0402](https://osv.dev/vulnerability/RUSTSEC-2026-0402), [RUSTSEC-2026-0403](https://osv.dev/vulnerability/RUSTSEC-2026-0403), [RUSTSEC-2026-0404](https://osv.dev/vulnerability/RUSTSEC-2026-0404), [RUSTSEC-2026-0405](https://osv.dev/vulnerability/RUSTSEC-2026-0405), [RUSTSEC-2026-0406](https://osv.dev/vulnerability/RUSTSEC-2026-0406)). Fixed by #93.
  <details>
  <summary>What they say</summary>

  - [RUSTSEC-2026-0395](https://osv.dev/vulnerability/RUSTSEC-2026-0395): Use-after-free with &#96;CParameter::StableInBuffer&#96;
  - [RUSTSEC-2026-0396](https://osv.dev/vulnerability/RUSTSEC-2026-0396): &#96;Seekable&#96; reads a NULL or out-of-bounds seek table
  - [RUSTSEC-2026-0397](https://osv.dev/vulnerability/RUSTSEC-2026-0397): &#96;OutBuffer::around&#95;pos&#96; exposes uninitialized memory
  - [RUSTSEC-2026-0398](https://osv.dev/vulnerability/RUSTSEC-2026-0398): &#96;Seekable::decompress&#96; sets a &#96;Vec&#96; length beyond its capacity for offsets past the end
  - [RUSTSEC-2026-0399](https://osv.dev/vulnerability/RUSTSEC-2026-0399): Use-after-free through &#96;init&#95;using&#95;cdict&#96; and &#96;init&#95;using&#95;ddict&#96;
  - [RUSTSEC-2026-0400](https://osv.dev/vulnerability/RUSTSEC-2026-0400): Use-after-free of a dictionary after a one-shot call on a context with an open stream
  - [RUSTSEC-2026-0401](https://osv.dev/vulnerability/RUSTSEC-2026-0401): Out-of-bounds read in &#96;train&#95;from&#95;buffer&#96; when sample sizes overflow
  - [RUSTSEC-2026-0402](https://osv.dev/vulnerability/RUSTSEC-2026-0402): Heap buffer overflow in &#96;CCtx::try&#95;clone&#96; on a multithreaded context
  - [RUSTSEC-2026-0403](https://osv.dev/vulnerability/RUSTSEC-2026-0403): Uninitialized memory exposed when streaming decompression of a legacy frame fails
  - [RUSTSEC-2026-0404](https://osv.dev/vulnerability/RUSTSEC-2026-0404): Double free in &#96;DCtx::try&#95;clone&#96;
  - [RUSTSEC-2026-0405](https://osv.dev/vulnerability/RUSTSEC-2026-0405): Out-of-bounds read with an out-of-range &#96;InBuffer::pos&#96;
  - [RUSTSEC-2026-0406](https://osv.dev/vulnerability/RUSTSEC-2026-0406): Use-after-free when changing a multithreaded context with an unfinished frame

  </details>

## Open PRs: patch

- #85: chore(deps): update `ctutils` to 0.4.4 &#91;security&#93;
- #93: chore(deps): update `zstd-safe` to 8.2.0 &#91;security&#93;
- #103: chore(deps): update `@astrojs/starlight` to 0.42.7

## Pending updates

- [ ] Create it now anyway: `@astrojs/starlight` → 0.42.8 (npm, `docs/package.json`; eligible 31 Oct 08:29 UTC) <!-- patchula-action:{"action":"create","target":"TARGET"} -->
- chore(deps): update `base64` to 0.23.1: lockfile generation failed: cargo: exit status: 101
  <details>
  <summary>The tool's output</summary>

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

## Lookup warnings and errors

- [ ] Try that lookup again: `patchula-cache` (crates, `crates/cli/Cargo.toml`, `crates/datasource/Cargo.toml`, `crates/http/Cargo.toml`) <!-- patchula-action:{"action":"retry","target":"TARGET"} -->
  - Lookup failed (crates, not-found): registry HTTP 404

<details>
<summary>Every dependency the Count found (10 in 7 files)</summary>

<details>
<summary>cargo / Cargo.lock (4 dependencies)</summary>

- `crypto-bigint`: 0.5.5
- `ctutils`: 0.4.3
- `unsafe-libyaml`: 0.2.11
- `zstd-safe`: 8.1.0

</details>

<details>
<summary>cargo / crates/cli/Cargo.toml (1 dependency)</summary>

- `patchula-cache`: 0.6.0

</details>

<details>
<summary>cargo / crates/datasource/Cargo.toml (1 dependency)</summary>

- `patchula-cache`: 0.6.0

</details>

<details>
<summary>cargo / crates/github/Cargo.toml (1 dependency)</summary>

- `rsa`: 0.9.10

</details>

<details>
<summary>cargo / crates/http/Cargo.toml (1 dependency)</summary>

- `patchula-cache`: 0.6.0

</details>

<details>
<summary>cargo / crates/server/Cargo.toml (1 dependency)</summary>

- `rsa`: 0.9.10

</details>

<details>
<summary>npm / docs/package.json (1 dependency)</summary>

- `@astrojs/starlight`: 0.42.6

</details>

</details>
