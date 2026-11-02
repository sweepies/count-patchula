import { defineRouteMiddleware } from "@astrojs/starlight/route-data";

/** Describes the software on the home page for search engines' structured data. */
export const onRequest = defineRouteMiddleware(({ locals, site }) => {
  if (locals.starlightRoute.id !== "") return;
  locals.starlightRoute.head.push({
    tag: "script",
    attrs: { type: "application/ld+json" },
    content: JSON.stringify({
      "@context": "https://schema.org",
      "@type": "SoftwareApplication",
      name: "Count Patchula",
      alternateName: "patchula",
      description:
        "A dependency update bot for GitHub that opens one pull request per update, with regenerated lockfiles, release notes and security advisories.",
      url: site?.href,
      image: new URL("/og.png", site).href,
      applicationCategory: "DeveloperApplication",
      operatingSystem: "Linux, macOS",
      license: "https://opensource.org/licenses/MIT",
      sameAs: "https://github.com/sweepies/count-patchula",
      offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
    }),
  });
});
