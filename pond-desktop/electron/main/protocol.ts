// Serving the renderer from a custom scheme rather than file://.
//
// `dist/index.html` references its bundle as `/assets/index-*.js` -- absolute,
// from the site root. That works over HTTP (which is how pond-server serves
// the same bundle on the Jetson) and it worked under `tauri://localhost`,
// which was a scheme origin with a root. Under `file://` it resolves to the
// filesystem root and the page loads nothing.
//
// The fix is not `base: './'` in vite.config: that would change the artifact
// pond-api embeds with include_dir!, so the desktop build and the LAN
// dashboard would stop being the same bytes. Instead we register a privileged
// standard scheme, which is the structural analogue of what Tauri did.
//
// It also gives the renderer a real, stable Origin, which is what the server's
// CORS allowlist needs to name. `file://` sends `Origin: null`, which cannot
// be allowlisted meaningfully and would push the server towards allowing any
// origin -- undoing the scoping that list exists for.

import { protocol, net } from "electron";
import { join, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

/** The scheme and host the renderer is served from. */
export const APP_SCHEME = "app";
export const APP_HOST = "giap";

/** The page origin, and the string the server's CORS allowlist must carry. */
export const APP_ORIGIN = `${APP_SCHEME}://${APP_HOST}`;

/**
 * Map a request path onto a file inside `distRoot`, or null if it escapes.
 *
 * The traversal guard is the point. This is the one place a string from the
 * page reaches the filesystem, and `..` segments in a URL survive
 * normalisation often enough to be worth refusing explicitly rather than
 * trusting the URL parser.
 */
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
 * Declare the scheme's privileges. Must run before the app is ready, which is
 * why it is separate from `serveRendererFrom`.
 *
 * `standard` is what gives the scheme a real origin at all; `secure` puts it
 * in a secure context so the Web Crypto the auth handshake uses is available;
 * `supportFetchAPI` lets the renderer fetch pond-server over HTTP.
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
    // Single-page app: an unknown path is a client route, not a missing file.
    // Anything under /assets/ genuinely missing should still 404, or a broken
    // bundle reference silently returns HTML and fails much later.
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
