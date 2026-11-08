import { readdir, readFile } from "node:fs/promises";
import { readFileSync } from "node:fs";
import { parse } from "smol-toml";

/**
 * The docs name the current release as `@@version@@`, filled in from the
 * workspace version in Cargo.toml when the site builds, so a release changes
 * Cargo.toml and nothing in the docs.
 */
export const TOKEN = "@@version@@";

const cargo = parse(readFileSync(new URL("../../../Cargo.toml", import.meta.url), "utf8"));
export const version = cargo.workspace.package.version;

const swap = (node, ctx) => {
  if (node.value.includes(TOKEN)) {
    ctx.replaceNode(node, { ...node, value: node.value.replaceAll(TOKEN, version) });
  }
};

/** A Sätteri mdast plugin for Markdown and MDX: prose, inline code and code blocks. */
export const versionPlugin = {
  name: "patchula-version",
  text: swap,
  inlineCode: swap,
  code: swap,
};

/** Fails the build when a built file still holds the token, from any source. */
export function versionCheck() {
  return {
    name: "patchula-version-check",
    hooks: {
      "astro:build:done": async ({ dir }) => {
        const left = [];
        for (const entry of await readdir(dir, { recursive: true, withFileTypes: true })) {
          if (!entry.isFile() || !/\.(html|md|txt|xml|json|js|css)$/.test(entry.name)) continue;
          const path = `${entry.parentPath}/${entry.name}`;
          if ((await readFile(path, "utf8")).includes(TOKEN)) left.push(path);
        }
        if (left.length) throw new Error(`${TOKEN} was not filled in:\n${left.join("\n")}`);
      },
    },
  };
}
