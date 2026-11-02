// Handing a file the page holds to the person: the shell shows its save dialog, a browser downloads it.

/** Keeps a name the pond or a title gave to one plain file name: no folders, no characters a disk refuses. */
function plainName(name: string): string | null {
  const base = name.split(/[\\/]/).pop() ?? "";
  const kept = Array.from(base)
    .filter((ch) => ch.charCodeAt(0) >= 32 && !'<>:"|?*'.includes(ch))
    .join("")
    .trim();
  return kept === "" ? null : kept;
}

/** The file name in a `Content-Disposition` header, or `null` when there is none the page can read. */
export function filenameFromDisposition(header: string | null): string | null {
  if (!header) return null;
  const extended = /filename\*\s*=\s*utf-8''([^;]+)/i.exec(header);
  if (extended) {
    try {
      const decoded = plainName(decodeURIComponent(extended[1].trim()));
      if (decoded) return decoded;
    } catch {
      // A malformed escape: fall through to the plain name.
    }
  }
  const quoted = /filename\s*=\s*"((?:[^"\\]|\\.)*)"/i.exec(header);
  if (quoted) return plainName(quoted[1].replace(/\\(.)/g, "$1"));
  const bare = /filename\s*=\s*([^;\s]+)/i.exec(header);
  return bare ? plainName(bare[1]) : null;
}

/** A file name from a title, for when the pond's header cannot be read: lower case, hyphens, no accents. */
export function fileNameFor(title: string, extension: string): string {
  const stem = title
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60)
    .replace(/-+$/, "");
  return `${stem === "" ? "report" : stem}.${extension}`;
}

/** Saves `blob` as `filename` through a download link, which is what the shell turns into its save dialog. */
export function saveBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.hidden = true;
  document.body.appendChild(link);
  link.click();
  link.remove();
  // Released later, not now: the browser reads the blob after the click returns.
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}
