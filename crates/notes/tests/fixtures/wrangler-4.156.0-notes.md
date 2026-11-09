#### [4.156.0](https://github.com/cloudflare/workers-sdk/releases/tag/wrangler@4.156.0)

### Minor Changes

- [#16532](https://redirect.github.com/cloudflare/workers-sdk/pull/16532) [`25802bc`](https://redirect.github.com/cloudflare/workers-sdk/commit/25802bce9f605916239bfd4714263c88f9b3098b) Thanks [@simonha9](https://github.com/simonha9)! - Allow the experimental `wrangler d1 insights` command to run across every D1 database in the account

  The database name is now optional. With no name, the command returns the top queries across every D1 database in the account, and each result gains a `databaseId` field.

### Patch Changes

- [#16369](https://redirect.github.com/cloudflare/workers-sdk/pull/16369) [`ec0bea4`](https://redirect.github.com/cloudflare/workers-sdk/commit/ec0bea48d6316c8ca893241efbf31f1ffcfa6134) Thanks [@MattieTK](https://github.com/MattieTK)! - Stop autoconfig in non-interactive runs when a project root holds both static files and a Pages `functions` directory

  When a project without a detected framework has a `functions` directory, autoconfig asks whether it is a Cloudflare Pages project. Non-interactive runs (CI and coding agents) assume it is not and continue as a static site. When the static files are in the project root, that deploys the root as static assets, and the Pages Functions (the project's API routes) are not deployed.

  In that case autoconfig now stops with an error explaining that Pages Functions cannot be migrated automatically yet. For Pages Functions it suggests moving the static files into a subdirectory and deploying with `wrangler pages deploy`, or migrating the Functions to a Worker; for a `functions` directory that is not Pages Functions, it suggests moving the static files into a subdirectory or adding a configuration file. Projects whose static files are already in a subdirectory, projects detected as Pages or with a detected framework, and interactive runs where the user answers the prompt are unaffected.

- [#16367](https://redirect.github.com/cloudflare/workers-sdk/pull/16367) [`b4653b4`](https://redirect.github.com/cloudflare/workers-sdk/commit/b4653b40cdc6e176c4e5ac98696cf0dae641ff6b) Thanks [@MattieTK](https://github.com/MattieTK)! - Write a protective `.assetsignore` when autoconfig uses the project root as the static assets directory

  Autoconfig is meant to write an `.assetsignore` when the project root itself is the assets directory, so that local files are not uploaded as public assets. The check compared the detected output directory, which is relative (`"."`), with the absolute project path, so it never matched and no `.assetsignore` was written. A static project with an `index.html` at its root (for example after `wrangler setup` or a zero-config `wrangler deploy`) then uploaded files such as `.env`, `.dev.vars`, `.git` and `node_modules` as public assets.

  The check now resolves the assets directory that the generated configuration uses and compares it with the project root, so frameworks that set their own build output directory (such as Nuxt or SvelteKit) do not get an unused root `.assetsignore`. The generated `.assetsignore` also covers what a Cloudflare Pages upload skips, alongside the existing `.wrangler`, `.dev.vars*` and `.env*` entries: `/_worker.js`, `/_routes.json` and `/functions` at the root (so nested paths such as `blog/functions/index.html` are still uploaded), `node_modules`, `.git` and `.DS_Store` at any depth, and `.npmrc`. If an existing `.assetsignore` only lists a narrower pattern such as `.env`, `.env*` is added so that files like `.env.production` are skipped too. When the project root of an assets-only project contains a Pages `functions` directory or `_worker.js`, autoconfig now warns that it will not be deployed, since it is excluded from the assets and not run as server code.

- [#16532](https://redirect.github.com/cloudflare/workers-sdk/pull/16532) [`25802bc`](https://redirect.github.com/cloudflare/workers-sdk/commit/25802bce9f605916239bfd4714263c88f9b3098b) Thanks [@simonha9](https://github.com/simonha9)! - Fail the experimental `wrangler d1 insights` command with an actionable error when the GraphQL Analytics API rejects the request

  Previously the command printed an empty `[]`. Rate limit errors now say how long to wait before retrying, and authorization errors name the "Account Analytics: Read" permission the API token needs.

- [#16013](https://redirect.github.com/cloudflare/workers-sdk/pull/16013) [`8b64bec`](https://redirect.github.com/cloudflare/workers-sdk/commit/8b64becbd3817abb28654e270c0e0104170f29d7) Thanks [@swalker326](https://github.com/swalker326)! - Fix Durable Object self-references in tests and container validation

  Durable Object bindings that explicitly name the Worker itself now resolve to the test runner in `@cloudflare/vitest-plugin`, for both `cloudflare.config.ts` and Wrangler configuration files. Named Wrangler environments use their effective Worker name, keeping bindings to the base Worker external. Container validation also accepts these self-references, using the effective deployment name to distinguish local and external bindings.

  Explicit binding targets remain intact during configuration conversion, so deploying with `--name` or a CI name override preserves the intended Durable Object target.

- Updated dependencies [[`15fbaed`](https://redirect.github.com/cloudflare/workers-sdk/commit/15fbaed2de992a529ca05d99424f8063d63217bf), [`d8899ac`](https://redirect.github.com/cloudflare/workers-sdk/commit/d8899ac4c85ff839d73daa9055a76a36248cd2e3)]:
  - miniflare@5.20261023.3-beta

#### [4.155.0](https://github.com/cloudflare/workers-sdk/releases/tag/wrangler@4.155.0)

### Minor Changes

- [#16536](https://redirect.github.com/cloudflare/workers-sdk/pull/16536) [`a95d647`](https://redirect.github.com/cloudflare/workers-sdk/commit/a95d647f9459e40a00871346b81082abbb27d2b3) Thanks [@WalshyDev](https://github.com/WalshyDev)! - Support remote Analytics Engine bindings in local development

  Setting `remote: true` on an Analytics Engine dataset now sends `writeDataPoint()` calls from `wrangler dev` and the Vite plugin to the real dataset. Without it, writes are still dropped locally.

  ```jsonc
  {
    "analytics_engine_datasets": [
      {
        "binding": "EVENTS",
        "dataset": "my-dataset",
        "remote": true
      }
    ]
  }
  ```

### Patch Changes

- [#16526](https://redirect.github.com/cloudflare/workers-sdk/pull/16526) [`dba0e81`](https://redirect.github.com/cloudflare/workers-sdk/commit/dba0e81501d3900907a6377dd88a153bb5139d37) Thanks [@petebacondarwin](https://github.com/petebacondarwin)! - Render terminal control characters as visible text in tail output

  Pretty tail and remote-preview output now escape control characters in log strings, exceptions and event metadata. Unicode, tabs and multiline logs remain readable, and JSON output keeps the original structured values.

- [#16489](https://redirect.github.com/cloudflare/workers-sdk/pull/16489) [`fc57bc0`](https://redirect.github.com/cloudflare/workers-sdk/commit/fc57bc0e03b096cff58e949e67ccfb9f7832a3c2) Thanks [@akshitsinha](https://github.com/akshitsinha)! - Keep Flagship flag metadata when updating flags

  `wrangler flagship flags` commands that update an existing flag, such as `enable`, `set`, `rollout`, `split`, and `rules`, no longer clear the flag's metadata. `wrangler flagship flags pull` now copies metadata into the local flag store.

- [#16521](https://redirect.github.com/cloudflare/workers-sdk/pull/16521) [`ec6eddd`](https://redirect.github.com/cloudflare/workers-sdk/commit/ec6edddadd8395b4991b23c5300ea68b701ce720) Thanks [@petebacondarwin](https://github.com/petebacondarwin)! - Validate `.assetsignore` encoding before building asset manifests

  Asset uploads and local development now stop with an actionable user error when `.assetsignore` is not valid UTF-8 or contains null characters, instead of silently losing its patterns. This also applies to standalone Miniflare and the Wrangler, Vite and Vitest development integrations. Save the file as UTF-8 to continue; UTF-8 files with a byte order mark and intentionally empty files remain supported.

- Updated dependencies [[`a95d647`](https://redirect.github.com/cloudflare/workers-sdk/commit/a95d647f9459e40a00871346b81082abbb27d2b3), [`c7a3373`](https://redirect.github.com/cloudflare/workers-sdk/commit/c7a3373cce06bdda0469d2b7d3d3286f3abbc3b1), [`c7a3373`](https://redirect.github.com/cloudflare/workers-sdk/commit/c7a3373cce06bdda0469d2b7d3d3286f3abbc3b1), [`c7a3373`](https://redirect.github.com/cloudflare/workers-sdk/commit/c7a3373cce06bdda0469d2b7d3d3286f3abbc3b1)]:
  - miniflare@5.20261023.2-beta

#### [4.154.0](https://github.com/cloudflare/workers-sdk/releases/tag/wrangler@4.154.0)

### Minor Changes

- [#16494](https://redirect.github.com/cloudflare/workers-sdk/pull/16494) [`0968b08`](https://redirect.github.com/cloudflare/workers-sdk/commit/0968b08c2a0b322fe6820bc2eb573440e6589a0b) Thanks [@petebacondarwin](https://github.com/petebacondarwin)! - Enable Miniflare's development registry by default and centralize path resolution

  In Miniflare v5 beta, resolve the registry from `CLOUDFLARE_DEV_REGISTRY_PATH`, then the `unsafeDevRegistryPath` constructor option, then `<Cloudflare config directory>/dev-registry/v5`. The default is independent of legacy Wrangler directories and is scoped to the Miniflare major version. The Vite v2 beta plugin follows this new default, while current Wrangler and `getPlatformProxy()` retain Wrangler's legacy default for compatibility with existing Wrangler sessions. Vite v2 and Wrangler therefore require the same `CLOUDFLARE_DEV_REGISTRY_PATH` to discover each other; set it for both processes when mixing these tools.

  Add `enableDevRegistry: false` to disable registration and discovery explicitly. Shared storage now uses the resolved default without requiring an explicit path. As before, shared storage requires an enabled registry to elect a storage owner, so combining `unsafeEnableSharedStorage: true` with `enableDevRegistry: false` is rejected. Miniflare no longer reads `MINIFLARE_REGISTRY_PATH`; use `CLOUDFLARE_DEV_REGISTRY_PATH` instead. Expose `resolveDevRegistryPath()` for integrations that also read registry files. Wrangler uses the effective path for its runtime configuration, initial bindings display, and Flagship active-session check, so the canonical override cannot bypass that check. An explicitly disabled Wrangler registry remains disabled.
