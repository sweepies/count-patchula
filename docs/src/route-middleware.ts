import { defineRouteMiddleware } from "@astrojs/starlight/route-data";
import { repo, versionLink, versionOf } from "./versions";

export const onRequest = defineRouteMiddleware(({ locals }) => {
  const route = locals.starlightRoute;
  const file = route.entry.filePath.replace(/^.*?content\/docs\//, "");
  const version = versionOf(file);

  // Every version edits the page on main.
  if (route.editUrl) {
    const page = version ? file.slice(version.length + 1) : file;
    route.editUrl = new URL(`${repo}/edit/main/docs/src/content/docs/${page}`);
  }
  if (!version) return;

  // Only the latest release is indexed.
  route.head.push({ tag: "meta", attrs: { name: "robots", content: "noindex" } });
  // Links stay inside the version: the site title, and the home page's buttons,
  // which come from front matter. Links in page content are moved at render
  // time, in astro.config.mjs.
  route.siteTitleHref = `/${version}/`;
  const hero = route.entry.data.hero;
  if (hero?.actions.length) {
    const actions = hero.actions.map((action) => ({ ...action, link: versionLink(action.link, version) }));
    route.entry = { ...route.entry, data: { ...route.entry.data, hero: { ...hero, actions } } };
  }
});
