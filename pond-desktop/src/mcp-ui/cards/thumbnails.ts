// Brave serves every thumbnail it returns through its own image proxy. Loading only those means a
// picture never tells the site it came from who is looking at it, so any other address is dropped.
const BRAVE_IMAGE_PROXY = "https://imgs.search.brave.com/";

/** The thumbnail when it is on Brave's image proxy, else null. */
export function braveThumbnail(raw: unknown): string | null {
  if (typeof raw !== "string" || !raw.startsWith(BRAVE_IMAGE_PROXY)) return null;
  try {
    const { origin, pathname } = new URL(raw);
    return origin === "https://imgs.search.brave.com" && pathname.length > 1 ? raw : null;
  } catch {
    return null;
  }
}

/** Only a web address is made a link; a page's own address never reaches `javascript:` or a file. */
export function webUrl(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  try {
    const { protocol } = new URL(raw);
    return protocol === "https:" || protocol === "http:" ? raw : null;
  } catch {
    return null;
  }
}

export function domainOf(url: string | null): string {
  if (!url) return "";
  return new URL(url).hostname.replace(/^www\./, "");
}

/** A string field of a card row, or "" when it is missing or not a string. */
export function text(raw: unknown): string {
  return typeof raw === "string" ? raw.trim() : "";
}
