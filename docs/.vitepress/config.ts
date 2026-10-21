import { defineConfig } from "vitepress";

const repo = "https://github.com/sweepies/count-patchula";

export default defineConfig({
  title: "Count Patchula",
  description:
    "A modern dependency updater with strong defaults. He never sleeps. Neither do your CVEs.",
  lang: "en-US",
  sitemap: { hostname: "https://count-patchula.sweepy.dev" },
  cleanUrls: true,
  lastUpdated: true,
  srcExclude: ["README.md"],
  head: [
    ["link", { rel: "icon", type: "image/png", href: "/favicon.png" }],
    ["link", { rel: "apple-touch-icon", href: "/apple-touch-icon.png" }],
    ["meta", { name: "theme-color", content: "#e85a4f" }],
    ["meta", { property: "og:type", content: "website" }],
    ["meta", { property: "og:title", content: "Count Patchula" }],
    ["meta", { property: "og:image", content: "/og.png" }],
    ["meta", { name: "twitter:card", content: "summary_large_image" }],
  ],
  markdown: {
    theme: { light: "github-light", dark: "github-dark" },
    // Shiki has no Pkl grammar. Swift shares its #"..."# raw strings and
    // highlights the rest closely enough.
    languageAlias: { pkl: "swift" },
  },
  themeConfig: {
    logo: { src: "/logo.png", alt: "Count Patchula, a bat holding a wrench" },
    siteTitle: "Count Patchula",
    outline: "deep",
    search: { provider: "local" },
    editLink: {
      pattern: `${repo}/edit/main/docs/:path`,
      text: "Edit this page on GitHub",
    },
    socialLinks: [{ icon: "github", link: repo }],
    footer: {
      message: "MIT licensed. Clean-room: no Renovate source was read in the making of this bat.",
      copyright: "Copyright © 2026 Sweepy",
    },
    nav: [
      { text: "Guide", link: "/getting-started", activeMatch: "^/(getting-started|installation|guide/)" },
      { text: "Coming from Renovate", link: "/renovate/compatibility", activeMatch: "^/renovate/" },
      { text: "Benchmarks", link: "/benchmarks" },
      {
        text: "Reference",
        activeMatch: "^/reference/",
        items: [
          { text: "CLI commands", link: "/reference/cli" },
          { text: "Patchula.pkl", link: "/reference/configuration" },
          { text: "Server settings", link: "/reference/server-settings" },
          { text: "Environment variables", link: "/reference/environment" },
          { text: "JSON report", link: "/reference/report" },
        ],
      },
    ],
    sidebar: [
      {
        text: "Start here",
        items: [
          { text: "Getting started", link: "/getting-started" },
          { text: "Installation", link: "/installation" },
          { text: "How it works", link: "/guide/how-it-works" },
        ],
      },
      {
        text: "Configure",
        items: [
          { text: "Configuration", link: "/guide/configuration" },
          { text: "Release age and channels", link: "/guide/lookup-policy" },
          { text: "Managers", link: "/guide/managers" },
          { text: "Sources", link: "/guide/sources" },
          { text: "Versioning", link: "/guide/versioning" },
        ],
      },
      {
        text: "Pull requests",
        items: [
          { text: "Branches and PRs", link: "/guide/pull-requests" },
          { text: "Lockfiles", link: "/guide/lockfiles" },
          { text: "Release notes", link: "/guide/release-notes" },
          { text: "Batching", link: "/guide/batch" },
          { text: "Dependency dashboard", link: "/guide/dashboard" },
          { text: "Auto-merge", link: "/guide/auto-merge" },
        ],
      },
      {
        text: "Run it",
        items: [
          { text: "Terminal UI", link: "/guide/tui" },
          { text: "GitHub App server", link: "/guide/server" },
        ],
      },
      {
        text: "Coming from Renovate",
        items: [
          { text: "Compatibility matrix", link: "/renovate/compatibility" },
          { text: "Migrating a config", link: "/renovate/migrating" },
          { text: "Deliberate departures", link: "/renovate/departures" },
        ],
      },
      {
        text: "About",
        items: [{ text: "Benchmarks", link: "/benchmarks" }],
      },
      {
        text: "Reference",
        items: [
          { text: "CLI commands", link: "/reference/cli" },
          { text: "Patchula.pkl", link: "/reference/configuration" },
          { text: "Server settings", link: "/reference/server-settings" },
          { text: "Environment variables", link: "/reference/environment" },
          { text: "JSON report", link: "/reference/report" },
        ],
      },
      {
        text: "Internals",
        collapsed: true,
        items: [
          { text: "Overview", link: "/internals/" },
          { text: "Design", link: "/internals/design" },
          { text: "Engine", link: "/internals/engine" },
          { text: "SQLite cache", link: "/internals/cache" },
          { text: "GitHub client", link: "/internals/github-client" },
          { text: "HTTP fixtures", link: "/internals/http-testing" },
          { text: "CI cache", link: "/internals/ci-cache" },
          {
            text: "Development log",
            collapsed: true,
            items: [
              { text: "Overview", link: "/internals/history/" },
              { text: "Phase 1 triage", link: "/internals/history/phase1-triage" },
              { text: "Phase 1, wave 2", link: "/internals/history/phase1-wave2" },
              { text: "Phase 2, wave 1", link: "/internals/history/phase2-wave1" },
              { text: "Phase 2, wave 2", link: "/internals/history/phase2-wave2" },
              { text: "Phase 2, wave 3", link: "/internals/history/phase2-wave3" },
              { text: "Benchmark history", link: "/internals/history/benchmarks" },
            ],
          },
        ],
      },
    ],
  },
});
