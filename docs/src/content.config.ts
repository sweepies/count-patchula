import { defineCollection } from "astro:content";
import { docsLoader, i18nLoader } from "@astrojs/starlight/loaders";
import { docsSchema, i18nSchema } from "@astrojs/starlight/schema";
import { slug } from "github-slugger";
import { versionOf } from "./versions";

export const collections = {
  docs: defineCollection({
    loader: docsLoader({
      // Astro's default ID, except that a version directory keeps its dots, so
      // an older line is served at /v0.1.x/ rather than /v01x/.
      generateId: ({ entry, data }) => {
        if (typeof data.slug === "string") return data.slug;
        const version = versionOf(entry);
        return entry
          .replace(/\.[^./]+$/, "")
          .split("/")
          .map((segment, index) => (index === 0 && segment === version ? segment : slug(segment)))
          .join("/")
          .replace(/\/index$/, "");
      },
    }),
    schema: docsSchema(),
  }),
  i18n: defineCollection({ loader: i18nLoader(), schema: i18nSchema() }),
};
