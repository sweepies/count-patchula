import { defineRouteMiddleware } from "@astrojs/starlight/route-data";

/** Links each docs page to the Markdown copy that `markdown-pages.mjs` writes next to it. */
export const onRequest = defineRouteMiddleware(({ locals, url }) => {
  if (url.pathname.startsWith("/404")) return;
  locals.starlightRoute.head.push({
    tag: "link",
    attrs: { rel: "alternate", type: "text/markdown", href: `${url.pathname.replace(/\/?$/, "/")}index.md` },
  });
});
