// Serves the renderer from app://giap, not file://: the bundle uses root-absolute /assets/
// paths, and `base: './'` would change the bytes pond-api embeds. It also gives a real
// Origin for the server's CORS allowlist, where file:// sends `Origin: null`.

import { protocol, net } from "electron";
import { join, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

/** The scheme and host the renderer is served from. */
export const APP_SCHEME = "app";
export const APP_HOST = "giap";

/** The page origin, and the string the server's CORS allowlist must carry. */
export const APP_ORIGIN = `${APP_SCHEME}://${APP_HOST}`;

/** Security: map a request path into `distRoot`, or null if `..` would escape it. */
export function resolveAppPath(
  distRoot: string,
  pathname: string,
): string | null {
  const root = resolve(distRoot);
  const rel =
    pathname === "/" || pathname === ""
      ? "index.html"
      : decodeURIComponent(pathname);
  const target = resolve(join(root, rel));
  if (target !== root && !target.startsWith(root + sep)) return null;
  return target;
}

/**
 * Declare the scheme's privileges; must run before the app is ready. `standard` gives a real
 * origin, `secure` enables the auth handshake's Web Crypto, `supportFetchAPI` allows fetch.
 */
export function registerAppScheme(): void {
  protocol.registerSchemesAsPrivileged([
    {
      scheme: APP_SCHEME,
      privileges: {
        standard: true,
        secure: true,
        supportFetchAPI: true,
        corsEnabled: true,
        stream: true,
      },
    },
  ]);
}

/** Start serving `distRoot` at app://giap. Call after the app is ready. */
export function serveRendererFrom(distRoot: string): void {
  protocol.handle(APP_SCHEME, async (request) => {
    const { pathname } = new URL(request.url);
    const file = resolveAppPath(distRoot, pathname);
    if (file === null) return new Response("forbidden", { status: 403 });

    const res = await net.fetch(pathToFileURL(file).toString());
    // SPA fallback to index.html, except /assets/: a missing bundle file must still 404.
    if (res.status === 404 && !pathname.startsWith("/assets/")) {
      const index = resolveAppPath(distRoot, "/");
      if (index !== null) return net.fetch(pathToFileURL(index).toString());
    }
    return res;
  });
}

/** The URL to load in the main window. */
export function rendererEntryUrl(): string {
  return `${APP_ORIGIN}/index.html`;
}
