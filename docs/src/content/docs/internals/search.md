---
title: "Site search"
description: How this site's search works, and what the DocSearch crawler needs.
---
The search box is [Algolia DocSearch](https://docsearch.algolia.com/) when the
build sees three variables, and Starlight's built-in
[Pagefind](https://pagefind.app/) index when it sees none of them:

| Variable | Value |
| --- | --- |
| `PUBLIC_DOCSEARCH_APP_ID` | The Algolia application ID |
| `PUBLIC_DOCSEARCH_API_KEY` | The search-only API key, which is public by design |
| `PUBLIC_DOCSEARCH_INDEX_NAME` | The index the crawler writes |

`astro.config.mjs` reads them when the site is built and adds the
`@astrojs/starlight-docsearch` plugin, which replaces the search component
and turns Pagefind off. Setting only some of them fails the build. Without
them, as in local builds and pull request checks, the site builds its own
Pagefind index and needs no network at search time.

The site is built by Cloudflare Workers Builds, which runs
`mise run docs:deploy` for `main` and `mise run docs:upload` for other
branches. The variables are build variables of the `count-patchula` Worker,
under Settings, Build, Variables and secrets. They are not `vars` in
`wrangler.jsonc`, which only reach the Worker at runtime. A new value takes
effect on the next build.

## The crawler

Algolia's crawler fills the index from the deployed site, not from the build.
It starts from the sitemap, which `@astrojs/sitemap` writes to
`/sitemap-index.xml`, so a page is searchable once it is deployed and the
crawler has run again.

Algolia publishes an
[Astro Starlight template](https://docsearch.algolia.com/docs/templates/#astro-starlight-template)
for the crawler. This site uses it with its own URL. The top level of a
result's hierarchy is the sidebar group that holds the page. Starlight writes
each group's label into two nested spans and nests subgroups inside their
parent's `<details>`, so the extractor reads only the `.large` span of the
outermost group, which `.first()` picks because matches come in document order:

```js
new Crawler({
  appId: "YOUR_APP_ID",
  apiKey: "YOUR_CRAWLER_API_KEY",
  rateLimit: 8,
  maxDepth: 10,
  startUrls: ["https://count-patchula.sweepy.dev/"],
  sitemaps: ["https://count-patchula.sweepy.dev/sitemap-index.xml"],
  ignoreCanonicalTo: true,
  discoveryPatterns: ["https://count-patchula.sweepy.dev/**"],
  actions: [
    {
      indexName: "YOUR_INDEX_NAME",
      pathsToMatch: ["https://count-patchula.sweepy.dev/**"],
      recordExtractor: ({ $, helpers }) => {
        const lvl0 =
          $('details:has(a[aria-current="page"])')
            .first()
            .children("summary")
            .find(".large")
            .first()
            .text()
            .trim() || "Documentation";
        return helpers.docsearch({
          recordProps: {
            lvl0: { selectors: "", defaultValue: lvl0 },
            lvl1: "main h1",
            lvl2: "main h2",
            lvl3: "main h3",
            lvl4: "main h4",
            lvl5: "main h5",
            lvl6: "main h6",
            content: "main p, main li",
          },
          indexHeadings: true,
          aggregateContent: true,
        });
      },
    },
  ],
  // initialIndexSettings: copy them from Algolia's template unchanged.
});
```

The crawler's own API key is the write key Algolia gives the crawler, not the
search-only key above, and it stays in Algolia.
