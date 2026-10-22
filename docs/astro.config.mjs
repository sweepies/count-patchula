import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig } from "astro/config";
import { satteri } from "@astrojs/markdown-satteri";
import sitemap from "@astrojs/sitemap";
import starlight from "@astrojs/starlight";
import starlightLinksValidator from "starlight-links-validator";
import starlightVersions from "starlight-versions";
import { repo, versionLabel, versionLink, versionOf } from "./src/versions.ts";

const site = "https://count-patchula.sweepy.dev";

// `mise run docs:assemble` lays out .site as the source directory: a
// copy of this checkout's src/, with the latest release's pages at the root of
// content/docs/ and this checkout's and each older minor line's in a directory
// of their own, plus each version's sidebar. `docs:dev` serves this
// checkout's src/ directly, without versions.
const dev = process.env.PATCHULA_DOCS_DEV === "1";
const src = dev ? "./src/" : "./.site/";
const read = (file) => readFileSync(new URL(file, import.meta.url), "utf8");
// Newest first: next, then the older lines.
const versions = dev
  ? []
  : readdirSync(new URL(`${src}content/versions/`, import.meta.url))
      .map((file) => file.replace(/\.json$/, ""))
      .sort((a, b) => (a === "next" ? -1 : b === "next" ? 1 : b.localeCompare(a, "en", { numeric: true })));

/** Links in a version's pages, other than the latest release's, stay in that version. */
function versionLinks({ fileURL }) {
  const version = fileURL && versionOf(fileURLToPath(fileURL).split("/content/docs/")[1] ?? "");
  if (!version) return null;
  const visit = (node, ctx) => {
    const href =
      node.type === "element"
        ? node.properties.href
        : node.attributes.find((attr) => attr.type === "mdxJsxAttribute" && attr.name === "href")?.value;
    if (typeof href === "string") ctx.setProperty(node, "href", versionLink(href, version));
  };
  return {
    name: "patchula-version-links",
    element: { filter: ["a"], visit },
    // Components such as <LinkCard> and <LinkButton>.
    mdxJsxFlowElement: { filter: [], visit },
    mdxJsxTextElement: { filter: [], visit },
  };
}

export default defineConfig({
  site,
  srcDir: src,
  markdown: {
    processor: satteri({
      // `### Heading {#id}` keeps the stable anchors other tools link to.
      features: { headingAttributes: true },
      hastPlugins: [versionLinks],
    }),
  },
  integrations: [
    // Only the latest release is indexed.
    sitemap({ filter: (page) => !versionOf(new URL(page).pathname) }),
    starlight({
      title: "Count Patchula",
      description:
        "A modern dependency updater with strong defaults. He never sleeps. Neither do your CVEs.",
      logo: { src: `${src}assets/logo.png`, alt: "Count Patchula, a bat holding a wrench" },
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
      ],
      social: [{ icon: "github", label: "GitHub", href: repo }],
      // The route middleware points every version's edit link at the page on main.
      editLink: { baseUrl: `${repo}/edit/main/docs/` },
      // Assembled pages are untracked, so git has no dates for them.
      lastUpdated: dev,
      // 404.astro renders the not-found page with the hero layout.
      disable404Route: true,
      customCss: ["@fontsource/righteous/latin-400.css", `${src}styles/custom.css`],
      components: {
        Footer: `${src}components/Footer.astro`,
        ...(dev ? {} : { ThemeSelect: `${src}components/ThemeSelect.astro` }),
      },
      routeMiddleware: `${src}route-middleware.ts`,
      expressiveCode: {
        themes: ["github-dark", "github-light"],
        // Code frames take the site's background and borders instead of the theme's.
        useStarlightUiThemeColors: true,
        styleOverrides: { borderRadius: "0.5rem" },
      },
      plugins: [
        starlightLinksValidator({
          // The validator slugifies paths itself and knows /v0.1.x/ only as
          // /v01x/. An older line's links were checked when it was the latest
          // release.
          exclude: ({ link }) => /^\/v\d+\.\d+\.x\//.test(link),
        }),
        ...(dev
          ? []
          : [
              starlightVersions({
                current: { label: `${read(`${src}current`).trim()} (latest)` },
                versions: versions.map((slug) => ({ slug, label: versionLabel(slug) })),
              }),
            ]),
      ],
      // Each version's own sidebar; the plugin reads the others' from content/versions/.
      sidebar: JSON.parse(read(dev ? "./sidebar.json" : `${src}sidebar.json`)),
    }),
  ],
});
