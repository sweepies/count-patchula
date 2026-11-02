import { existsSync, readFileSync } from "node:fs";
import { defineConfig } from "astro/config";
import { satteri } from "@astrojs/markdown-satteri";
import sitemap from "@astrojs/sitemap";
import starlight from "@astrojs/starlight";
import starlightDocSearch from "@astrojs/starlight-docsearch";
import starlightLinksValidator from "starlight-links-validator";
import starlightLlmsTxt from "starlight-llms-txt";
import markdownPages from "./src/plugins/markdown-pages.mjs";

const site = "https://count-patchula.sweepy.dev";
const repo = "https://github.com/sweepies/count-patchula";
const raw = "https://raw.githubusercontent.com/sweepies/count-patchula/main";
const read = (file) => readFileSync(new URL(file, import.meta.url), "utf8");

/**
 * The agent setup instructions and the patchula skill are fetched from the
 * repository, not the site, so the links validator never sees them. Their links
 * to the site must reach a page, and a heading on it, or a file such as
 * llms.txt, at the root, which is what they link to.
 */
function agentLinks() {
  const files = ["./agent-setup.md", "../skills/patchula/SKILL.md"];
  const url = new RegExp(`${site.replace(/[.]/g, "\\.")}(/(?:[^\\s)]*[^\\s).,])?)?`, "g");
  return {
    name: "patchula-agent-links",
    hooks: {
      "astro:build:done": ({ dir }) => {
        const broken = [];
        for (const file of files) {
          for (const [link, href = "/"] of read(file).matchAll(url)) {
            const [path, anchor] = href.split("#");
            // A file such as /llms.txt is checked as it is; a page as its index.html.
            const target = /\.\w+$/.test(path) ? path : `${path.replace(/\/?$/, "/")}index.html`;
            const page = new URL(`.${target}`, dir);
            if (!existsSync(page) || (anchor && !readFileSync(page, "utf8").includes(` id="${anchor}"`))) {
              broken.push(`${file}: ${link}`);
            }
          }
        }
        if (broken.length) throw new Error(`Broken links to the site:\n${broken.join("\n")}`);
      },
    },
  };
}

/**
 * Search is Algolia DocSearch when all three values are set at build time, and
 * Starlight's Pagefind index otherwise. The search-only API key is public by
 * design. Setting only some of them is a mistake, so it fails the build.
 */
function docSearch() {
  const env = {
    appId: "PUBLIC_DOCSEARCH_APP_ID",
    apiKey: "PUBLIC_DOCSEARCH_API_KEY",
    indexName: "PUBLIC_DOCSEARCH_INDEX_NAME",
  };
  const options = Object.fromEntries(Object.entries(env).map(([key, name]) => [key, process.env[name]]));
  const missing = Object.entries(env).filter(([key]) => !options[key]);
  if (missing.length === 3) return [];
  if (missing.length) {
    throw new Error(`DocSearch needs all three variables; missing ${missing.map(([, name]) => name).join(", ")}`);
  }
  return [starlightDocSearch(options)];
}

const sidebar = [
  {
    label: "Start here",
    items: [
      { label: "Getting started", slug: "getting-started" },
      { label: "Installation", slug: "installation" },
      { label: "How it works", slug: "guide/how-it-works" },
    ],
  },
  {
    label: "Configure",
    items: [
      { label: "Configuration", slug: "guide/configuration" },
      { label: "Release age and channels", slug: "guide/lookup-policy" },
      { label: "Security advisories", slug: "guide/security" },
      { label: "Managers", slug: "guide/managers" },
      { label: "Sources", slug: "guide/sources" },
      { label: "Versioning", slug: "guide/versioning" },
    ],
  },
  {
    label: "Pull requests",
    items: [
      { label: "Branches and PRs", slug: "guide/pull-requests" },
      { label: "Lockfiles", slug: "guide/lockfiles" },
      { label: "Release notes", slug: "guide/release-notes" },
      { label: "Batching", slug: "guide/batch" },
      { label: "Dependency dashboard", slug: "guide/dashboard" },
      { label: "Auto-merge", slug: "guide/auto-merge" },
    ],
  },
  {
    label: "Run it",
    items: [
      { label: "GitHub Actions", slug: "guide/github-actions" },
      { label: "Terminal UI", slug: "guide/tui" },
      { label: "Applying updates locally", slug: "guide/apply" },
      {
        label: "GitHub App server",
        slug: "guide/server",
        badge: { text: "Recommended", variant: "default" },
      },
    ],
  },
  {
    label: "Coming from Renovate",
    items: [
      { label: "Compatibility matrix", slug: "renovate/compatibility" },
      { label: "Migrating a config", slug: "renovate/migrating" },
      { label: "Departures", slug: "renovate/departures" },
    ],
  },
  { label: "About", items: [{ label: "Benchmarks", slug: "benchmarks" }] },
  {
    label: "Reference",
    items: [
      { label: "CLI commands", slug: "reference/cli" },
      { label: "Configuration schema", slug: "reference/configuration" },
      { label: "Settings", slug: "reference/settings" },
      { label: "Environment variables", slug: "reference/environment" },
      { label: "JSON report", slug: "reference/report" },
    ],
  },
  {
    label: "Internals",
    collapsed: true,
    items: [
      { label: "Overview", slug: "internals" },
      { label: "Design", slug: "internals/design" },
      { label: "Engine", slug: "internals/engine" },
      { label: "SQLite cache", slug: "internals/cache" },
      { label: "GitHub client", slug: "internals/github-client" },
      { label: "Image SBOM advisories", slug: "internals/image-sboms" },
      { label: "HTTP fixtures", slug: "internals/http-testing" },
    ],
  },
];

/**
 * The page index at the top of llms.txt: every sidebar page, by group, linked
 * to its Markdown copy and described by its frontmatter description.
 */
function pageIndex() {
  const description = (slug) => {
    const file = [".md", ".mdx", "/index.md", "/index.mdx"]
      .map((ext) => new URL(`./src/content/docs/${slug}${ext}`, import.meta.url))
      .find(existsSync);
    const match = file && readFileSync(file, "utf8").match(/^description: "?(.*?)"?$/m);
    if (!match) throw new Error(`llms.txt: ${slug} has no description`);
    return match[1];
  };
  return sidebar
    .map(({ label, items }) => {
      const pages = items.map(
        (item) => `  - [${item.label}](${site}/${item.slug}/index.md): ${description(item.slug)}`,
      );
      return [`- ${label}`, ...pages].join("\n");
    })
    .join("\n");
}

export default defineConfig({
  site,
  markdown: {
    processor: satteri({
      // `### Heading {#id}` keeps the stable anchors other tools link to.
      features: { headingAttributes: true },
    }),
  },
  integrations: [
    agentLinks(),
    sitemap(),
    starlight({
      title: "Count Patchula",
      description:
        "A modern dependency updater with strong defaults. He never sleeps. Neither do your CVEs.",
      logo: { src: "./src/assets/logo.png", alt: "Count Patchula, a bat holding a wrench" },
      favicon: "/favicon.png",
      head: [
        { tag: "link", attrs: { rel: "apple-touch-icon", href: "/apple-touch-icon.png" } },
        { tag: "meta", attrs: { name: "theme-color", content: "#e85a4f" } },
        // Link previews only load absolute image URLs.
        { tag: "meta", attrs: { property: "og:image", content: `${site}/og.png` } },
        { tag: "meta", attrs: { property: "og:image:width", content: "1200" } },
        { tag: "meta", attrs: { property: "og:image:height", content: "630" } },
        {
          tag: "meta",
          attrs: {
            property: "og:image:alt",
            content: "Count Patchula, a bat holding a wrench: vants to update your deps.",
          },
        },
        { tag: "meta", attrs: { name: "twitter:image", content: `${site}/og.png` } },
        { tag: "meta", attrs: { name: "algolia-site-verification", content: "00975EB341369C04" } },
      ],
      social: [{ icon: "github", label: "GitHub", href: repo }],
      editLink: { baseUrl: `${repo}/edit/main/docs/` },
      // 404.astro renders the not-found page with the hero layout.
      disable404Route: true,
      customCss: ["@fontsource/righteous/latin-400.css", "./src/styles/custom.css"],
      components: { Footer: "./src/components/Footer.astro" },
      expressiveCode: {
        themes: ["github-dark", "github-light"],
        // Code frames take the site's background and borders instead of the theme's.
        useStarlightUiThemeColors: true,
        styleOverrides: { borderRadius: "0.5rem" },
      },
      routeMiddleware: "./src/routeData.ts",
      plugins: [
        starlightLinksValidator(),
        ...docSearch(),
        markdownPages(),
        starlightLlmsTxt({
          description:
            "Count Patchula is a dependency update bot for GitHub, written in Rust, and an alternative to Renovate. The binary is `patchula`. It opens one pull request per update, with regenerated lockfiles, release notes and security advisories, and is configured with a `patchula.pkl` file in Pkl.",
          details: [
            "Every page is also served as Markdown: request its URL with `Accept: text/markdown`, or add `index.md` to it.",
            "",
            `A coding agent setting the Count up in a repository should follow the [agent setup instructions](${raw}/docs/agent-setup.md).`,
            "",
            pageIndex(),
          ].join("\n"),
          optionalLinks: [
            {
              label: "patchula skill",
              url: `${raw}/skills/patchula/SKILL.md`,
              description: "the agent skill for using the patchula CLI",
            },
            { label: "Source code", url: repo },
          ],
          customSets: [
            {
              label: "Guides",
              paths: ["getting-started", "installation", "guide/**"],
              description: "installing, configuring and running the Count",
            },
            {
              label: "Reference",
              paths: ["reference/**"],
              description: "the CLI, the configuration and settings schemas, environment variables and the JSON report",
            },
            {
              label: "Coming from Renovate",
              paths: ["renovate/**"],
              description: "the compatibility matrix, migrating a config, and departures from Renovate",
            },
            {
              label: "Internals",
              paths: ["internals/**"],
              description: "architecture, cache and HTTP test design, for contributors",
            },
          ],
          // The abridged set leaves out contributor-facing pages.
          exclude: ["internals/**", "benchmarks"],
          demote: ["internals/**"],
        }),
      ],
      sidebar,
    }),
  ],
});
