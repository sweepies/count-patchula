import { mkdir, readFile, writeFile } from "node:fs/promises";
import { matches, select, selectAll } from "hast-util-select";
import rehypeParse from "rehype-parse";
import rehypeRemark from "rehype-remark";
import remarkGfm from "remark-gfm";
import remarkStringify from "remark-stringify";
import { unified } from "unified";
import { remove } from "unist-util-remove";

/**
 * Writes a Markdown copy of every docs page next to its HTML, at
 * `/<slug>/index.md`, and links it from the page's head.
 *
 * The copy is converted from the built HTML rather than the source, so MDX
 * components and imported files (the Pkl schemas on the reference pages) come
 * through as they render. The site's Worker serves these files to requests
 * that prefer `text/markdown`.
 */
export default function markdownPages() {
  return {
    name: "patchula-markdown-pages",
    hooks: {
      "config:setup"({ addIntegration, addRouteMiddleware, astroConfig }) {
        addRouteMiddleware({ entrypoint: "./src/plugins/markdown-pages-head.ts" });
        addIntegration({
          name: "patchula-markdown-pages",
          hooks: {
            "astro:build:done": async ({ dir, pages, logger }) => {
              let written = 0;
              for (const { pathname } of pages) {
                const page = new URL(`./${pathname}`.replace(/\/?$/, "/"), dir);
                const html = await readFile(new URL("index.html", page), "utf8").catch(() => null);
                const markdown = html && toMarkdown(html, new URL(pathname, astroConfig.site));
                if (!markdown) continue;
                await mkdir(page, { recursive: true });
                await writeFile(new URL("index.md", page), markdown);
                written++;
              }
              logger.info(`Wrote ${written} Markdown pages`);
            },
          },
        });
      },
    },
  };
}

const parse = unified().use(rehypeParse);
const stringify = unified()
  .use(rehypeRemark)
  .use(remarkGfm, { tablePipeAlign: false })
  .use(remarkStringify, { bullet: "-", rule: "-" });

/** Converts a built page to Markdown, or returns null for a page with no docs content. */
function toMarkdown(html, url) {
  const document = parse.parse(html);
  const content = select(".sl-markdown-content", document);
  if (!content || select('meta[name="robots"][content*="noindex"]', document)) return null;

  const title = text(select("h1#_top", document) ?? select("title", document));
  const description = select('meta[name="description"]', document)?.properties.content;

  remove(content, (node) => {
    if (node.type === "comment") return true;
    return ["script", "style", "link", "svg", ".sl-anchor-link", ".copy", ".sr-only"].some((s) =>
      matches(s, node),
    );
  });
  for (const link of selectAll("a[href], img[src]", content)) {
    const key = link.tagName === "a" ? "href" : "src";
    const value = String(link.properties[key]);
    if (value.startsWith("/")) link.properties[key] = new URL(value, url).href;
  }
  for (const title of selectAll("article.card > .title", content)) title.tagName = "h3";
  codeBlocks(content);
  asides(content);
  tabs(content);

  const body = String(stringify.stringify(stringify.runSync({ type: "root", children: [content] }))).trim();
  const lead = description ? `\n\n> ${description}` : "";
  return `# ${title}${lead}\n\n${body}\n`;
}

/** Expressive Code frames: the language goes on the code, the title before it. */
function codeBlocks(tree) {
  for (const frame of selectAll(".expressive-code", tree)) {
    const pre = select("pre", frame);
    const code = select("code", frame);
    if (!pre || !code) continue;
    if (pre.properties.dataLanguage) code.properties.className = [`language-${pre.properties.dataLanguage}`];
    // One div per line, and an empty line's div holds a newline of its own.
    const lines = selectAll(".ec-line", code).map((line) => textContent(line).replace(/\n$/, ""));
    code.children = [{ type: "text", value: lines.join("\n") }];
    const title = text(select("figcaption .title", frame));
    frame.tagName = "div";
    frame.children = [
      ...(title ? [element("p", [element("code", [{ type: "text", value: title }])])] : []),
      pre,
    ];
  }
}

/** Starlight asides become blockquotes that open with their title. */
function asides(tree) {
  for (const aside of selectAll("aside.starlight-aside", tree)) {
    const title = text(select(".starlight-aside__title", aside));
    const content = select(".starlight-aside__content", aside);
    aside.tagName = "blockquote";
    aside.children = [element("p", [element("strong", [{ type: "text", value: title }])]), ...(content?.children ?? [])];
  }
}

/** Tabs become a list with one item per tab, labelled with the tab's name. */
function tabs(tree) {
  for (const instance of selectAll("starlight-tabs", tree)) {
    const labels = selectAll('[role="tab"]', instance).map(text);
    const panels = selectAll('[role="tabpanel"]', instance);
    instance.tagName = "ul";
    instance.properties = {};
    instance.children = panels.map((panel, i) =>
      element("li", [element("p", [element("strong", [{ type: "text", value: labels[i] ?? "" }])]), panel]),
    );
  }
}

function element(tagName, children) {
  return { type: "element", tagName, properties: {}, children };
}

function text(node) {
  return node ? textContent(node).trim() : "";
}

function textContent(node) {
  if (node.type === "text") return node.value;
  return (node.children ?? []).map(textContent).join("");
}
