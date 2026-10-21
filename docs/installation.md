# Installation

The Count is one binary, `patchula`. There are no prebuilt releases yet, so
build him with Cargo.

## With Cargo

```sh
cargo install --git https://github.com/sweepies/count-patchula --locked patchula-cli
```

This needs Rust 1.99 or newer and a C linker (`cc`). The repository's own
build setup, which supplies a Zig linker through mise, is not used by
`cargo install`.

## From a checkout

```sh
git clone https://github.com/sweepies/count-patchula
cd count-patchula
mise trust && mise install
mise run build
./target/debug/patchula --version
```

Every tool version is pinned in `mise.toml`, including Rust and the Zig linker,
so a minimal image without system development packages can build it.

## What the runner needs

Lockfiles are regenerated with the real tools (aube, Yarn, Bun, Cargo, Go,
Terraform, uv, Poetry, dotnet, Bundler, Composer), so `mise` must be on the
`PATH`. mise installs each tool on first use into the Count's own tool store,
never your personal mise installs. The store is `PATCHULA_TOOLS_DIR`, or
`<cache>.tools` beside the SQLite cache. Put it on a persistent volume.

`git` is needed for the partial clones a lockfile tool works in.

The cache path must be writable. The default is `.patchula-cache.sqlite` in the
working directory. It holds HTTP validators, catalogs, write receipts and the
partial clones (`<cache>.worktrees`). A warm cache is most of the speed, so
keep it between runs.

Some tools download slowly. The static PHP build has been measured at about
44 KB/s, so prewarm the store when you build an image:

```sh
patchula tools install --cache /var/lib/patchula/cache.sqlite
patchula tools prune --days 30 --cache /var/lib/patchula/cache.sqlite
```

`tools install` installs every pinned default. `tools prune` removes versions
no run has used for that many days. Pinned defaults are never pruned.

## Shell completions and man page

`mise run docs:cli` in a checkout generates `generated/patchula.1` and bash, zsh
and fish completions under `generated/completions`, along with the [CLI
reference](/reference/cli).
