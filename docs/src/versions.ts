// `mise run docs:assemble` puts the latest release's pages at the
// root of the docs collection and every other version's in a directory named
// after it: `next` for this checkout, `vX.Y.x` for an older minor line.

export const repo = "https://github.com/sweepies/count-patchula";

const versionDir = /^(next|v\d+\.\d+\.x)(?:\/|$)/;

/** The version a docs entry ID or path is in, or undefined for the latest release. */
export function versionOf(path: string): string | undefined {
  return versionDir.exec(path.replace(/^\//, ""))?.[1];
}

/** The picker's label for a version directory. */
export function versionLabel(version: string): string {
  return version === "next" ? "Next (unreleased)" : version;
}

/** A site link from one of a version's pages, moved into that version. */
export function versionLink(href: string, version: string): string {
  if (!href.startsWith("/") || href.startsWith("//") || versionOf(href)) return href;
  // Files such as /og.png are shared by every version.
  const last = href.split(/[?#]/)[0].split("/").pop() ?? "";
  if (/\.[a-z0-9]+$/i.test(last)) return href;
  return `/${version}${href}`;
}
