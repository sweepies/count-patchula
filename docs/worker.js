// Serves the site's static assets, and each page's Markdown copy (written at
// build time as <page>/index.md) to requests that prefer text/markdown.
// Requests for _astro/ and pagefind/ files go straight to the assets; see
// run_worker_first in wrangler.jsonc.

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/") && prefersMarkdown(request.headers.get("Accept"))) {
      const markdown = await env.ASSETS.fetch(new URL(`${url.pathname}index.md`, url), request);
      // A 304 answers a conditional request for the Markdown copy itself.
      if (markdown.ok || markdown.status === 304) {
        const response = negotiated(markdown);
        response.headers.set("Content-Type", "text/markdown; charset=utf-8");
        return response;
      }
    }
    const response = await env.ASSETS.fetch(request);
    return response.headers.get("Content-Type")?.startsWith("text/html") ? negotiated(response) : response;
  },
};

/** A copy of the response that caches keep apart by Accept. */
function negotiated(response) {
  const copy = new Response(response.body, response);
  copy.headers.append("Vary", "Accept");
  return copy;
}

/** Whether an Accept header ranks text/markdown at least as high as text/html. */
export function prefersMarkdown(accept) {
  if (!accept) return false;
  const quality = {};
  for (const range of accept.split(",")) {
    const [type, ...params] = range.split(";").map((part) => part.trim().toLowerCase());
    const q = params.find((param) => param.startsWith("q="));
    quality[type] = q ? Number(q.slice(2)) : 1;
  }
  const markdown = quality["text/markdown"] ?? 0;
  return markdown > 0 && markdown >= (quality["text/html"] ?? quality["text/*"] ?? quality["*/*"] ?? 0);
}
